//! Sessions and the agent loop.
//!
//! A [`Session`] owns an append-only event log, mirrored to
//! `<data_dir>/sessions/<id>/events.jsonl`. The daemon, not any client, runs
//! the loop: a turn keeps going when every browser is closed, and a client
//! that reconnects replays what it missed.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot};
use unchained_protocol::{Event, EventKind, SessionInfo, WorkspaceInfo};

use crate::model::ModelAdapter;
use crate::permissions::{Policy, Verdict};
use crate::tools::ToolRegistry;
use crate::workspace::Workspace;

/// Model round-trips allowed in one turn before the loop stops itself.
const MAX_STEPS: usize = 32;
/// An unanswered permission request is denied after this long.
const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BROADCAST_CAPACITY: usize = 1024;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SubmitError {
    #[error("a turn is already running in this session")]
    Busy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionMeta {
    id: String,
    workspace: String,
    created_ms: u64,
}

struct Deps {
    model: Arc<dyn ModelAdapter>,
    tools: Arc<ToolRegistry>,
    policy: Policy,
    approval_timeout: Duration,
}

pub struct Session {
    meta: SessionMeta,
    workspace: Workspace,
    deps: Arc<Deps>,
    events: Mutex<Vec<Event>>,
    tx: broadcast::Sender<Event>,
    pending: Mutex<HashMap<String, oneshot::Sender<(bool, &'static str)>>>,
    running: AtomicBool,
    cancel: AtomicBool,
    log_path: PathBuf,
}

impl Session {
    pub fn id(&self) -> &str {
        &self.meta.id
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.meta.id.clone(),
            workspace: self.meta.workspace.clone(),
            created_ms: self.meta.created_ms,
            running: self.running.load(Ordering::SeqCst),
            last_seq: self.events.lock().unwrap().len() as u64,
        }
    }

    /// Events with `seq > after`, plus a receiver for everything later.
    /// Taken under the log lock, so there is no gap and no overlap.
    pub fn subscribe_from(&self, after: u64) -> (Vec<Event>, broadcast::Receiver<Event>) {
        let events = self.events.lock().unwrap();
        let start = (after as usize).min(events.len());
        (events[start..].to_vec(), self.tx.subscribe())
    }

    fn emit(&self, kind: EventKind) {
        let mut events = self.events.lock().unwrap();
        let event = Event {
            seq: events.len() as u64 + 1,
            ts_ms: now_ms(),
            kind,
        };
        if let Err(e) = append_line(&self.log_path, &event) {
            tracing::error!(session = %self.meta.id, "failed to persist event: {e:#}");
        }
        events.push(event.clone());
        let _ = self.tx.send(event);
    }

    /// Accept a user message and run the agent loop in the background.
    pub fn submit_user_message(self: &Arc<Self>, text: String) -> Result<(), SubmitError> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(SubmitError::Busy);
        }
        self.cancel.store(false, Ordering::SeqCst);
        self.emit(EventKind::UserMessage { text });
        let session = Arc::clone(self);
        tokio::spawn(async move {
            session.run_turn().await;
            // Clear `running` before announcing the end of the turn, so a
            // client reacting to TurnFinished is never told "busy".
            session.running.store(false, Ordering::SeqCst);
            session.emit(EventKind::TurnFinished);
        });
        Ok(())
    }

    /// Answer a pending permission request. Returns false if none matches.
    pub fn resolve_permission(&self, call_id: &str, allow: bool) -> bool {
        match self.pending.lock().unwrap().remove(call_id) {
            Some(tx) => tx.send((allow, "client")).is_ok(),
            None => false,
        }
    }

    /// Stop the current turn at the next step and deny anything pending.
    pub fn interrupt(&self) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        self.cancel.store(true, Ordering::SeqCst);
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send((false, "interrupt"));
        }
    }

    async fn run_turn(&self) {
        for _ in 0..MAX_STEPS {
            if self.cancel.load(Ordering::SeqCst) {
                self.emit(EventKind::Error {
                    message: "interrupted".into(),
                });
                return;
            }
            let history = self.events.lock().unwrap().clone();
            let turn = match self.deps.model.next_turn(&history).await {
                Ok(t) => t,
                Err(e) => {
                    self.emit(EventKind::Error {
                        message: format!("model error: {e:#}"),
                    });
                    return;
                }
            };
            if !turn.text.is_empty() {
                self.emit(EventKind::AssistantMessage { text: turn.text });
            }
            if turn.tool_calls.is_empty() {
                return;
            }
            for request in turn.tool_calls {
                if self.cancel.load(Ordering::SeqCst) {
                    break;
                }
                self.run_tool(request.tool, request.input).await;
            }
        }
        self.emit(EventKind::Error {
            message: format!("stopped after {MAX_STEPS} steps"),
        });
    }

    async fn run_tool(&self, tool_name: String, input: serde_json::Value) {
        let call_id = uuid::Uuid::new_v4().simple().to_string();
        self.emit(EventKind::ToolCall {
            call_id: call_id.clone(),
            tool: tool_name.clone(),
            input: input.clone(),
        });
        let result = |ok: bool, output: String| EventKind::ToolResult {
            call_id: call_id.clone(),
            ok,
            output,
        };
        let Some(tool) = self.deps.tools.get(&tool_name) else {
            self.emit(result(false, format!("unknown tool '{tool_name}'")));
            return;
        };
        let allowed = match self.deps.policy.decide(&tool_name, tool.effect()) {
            Verdict::Allow => true,
            Verdict::Deny => false,
            Verdict::Ask => self.ask(&call_id, &tool_name, &input).await,
        };
        if !allowed {
            self.emit(result(false, "permission denied".into()));
            return;
        }
        match tool.run(&self.workspace, &input).await {
            Ok(output) => self.emit(result(true, output)),
            Err(e) => self.emit(result(false, format!("{e:#}"))),
        }
    }

    async fn ask(&self, call_id: &str, tool: &str, input: &serde_json::Value) -> bool {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(call_id.to_string(), tx);
        self.emit(EventKind::PermissionRequested {
            call_id: call_id.to_string(),
            tool: tool.to_string(),
            input: input.clone(),
        });
        let (allowed, by) = match tokio::time::timeout(self.deps.approval_timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => (false, "interrupt"),
            Err(_) => {
                self.pending.lock().unwrap().remove(call_id);
                (false, "timeout")
            }
        };
        self.emit(EventKind::PermissionResolved {
            call_id: call_id.to_string(),
            allowed,
            by: by.to_string(),
        });
        allowed
    }
}

fn append_line(path: &Path, event: &Event) -> Result<()> {
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Owns every session and the workspaces they may run in.
pub struct SessionManager {
    data_dir: PathBuf,
    workspaces: Vec<Workspace>,
    deps: Arc<Deps>,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

impl SessionManager {
    /// Open `data_dir`, restoring any sessions persisted there.
    pub fn open(
        data_dir: impl Into<PathBuf>,
        workspaces: Vec<Workspace>,
        policy: Policy,
        model: Arc<dyn ModelAdapter>,
        tools: Arc<ToolRegistry>,
    ) -> Result<Self> {
        Self::open_with_timeout(
            data_dir,
            workspaces,
            policy,
            model,
            tools,
            DEFAULT_APPROVAL_TIMEOUT,
        )
    }

    pub fn open_with_timeout(
        data_dir: impl Into<PathBuf>,
        workspaces: Vec<Workspace>,
        policy: Policy,
        model: Arc<dyn ModelAdapter>,
        tools: Arc<ToolRegistry>,
        approval_timeout: Duration,
    ) -> Result<Self> {
        let data_dir = data_dir.into();
        fs::create_dir_all(data_dir.join("sessions"))
            .with_context(|| format!("creating {}", data_dir.display()))?;
        let manager = Self {
            data_dir,
            workspaces,
            deps: Arc::new(Deps {
                model,
                tools,
                policy,
                approval_timeout,
            }),
            sessions: RwLock::new(HashMap::new()),
        };
        manager.restore()?;
        Ok(manager)
    }

    pub fn workspaces(&self) -> Vec<WorkspaceInfo> {
        self.workspaces
            .iter()
            .map(|w| WorkspaceInfo {
                name: w.name().to_string(),
                root: w.root().display().to_string(),
            })
            .collect()
    }

    fn workspace(&self, name: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.name() == name)
    }

    pub fn create(&self, workspace: &str) -> Result<Arc<Session>> {
        let ws = self
            .workspace(workspace)
            .ok_or_else(|| anyhow!("unknown workspace '{workspace}'"))?;
        let meta = SessionMeta {
            id: uuid::Uuid::new_v4().simple().to_string(),
            workspace: ws.name().to_string(),
            created_ms: now_ms(),
        };
        let dir = self.data_dir.join("sessions").join(&meta.id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
        let session = self.build(meta, ws.clone(), Vec::new(), dir.join("events.jsonl"));
        self.sessions
            .write()
            .unwrap()
            .insert(session.id().to_string(), Arc::clone(&session));
        Ok(session)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.read().unwrap().get(id).cloned()
    }

    /// Newest first.
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut infos: Vec<_> = self
            .sessions
            .read()
            .unwrap()
            .values()
            .map(|s| s.info())
            .collect();
        infos.sort_by_key(|s| std::cmp::Reverse(s.created_ms));
        infos
    }

    fn build(
        &self,
        meta: SessionMeta,
        workspace: Workspace,
        events: Vec<Event>,
        log_path: PathBuf,
    ) -> Arc<Session> {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Arc::new(Session {
            meta,
            workspace,
            deps: Arc::clone(&self.deps),
            events: Mutex::new(events),
            tx,
            pending: Mutex::new(HashMap::new()),
            running: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            log_path,
        })
    }

    fn restore(&self) -> Result<()> {
        for entry in fs::read_dir(self.data_dir.join("sessions"))? {
            let dir = entry?.path();
            match self.restore_one(&dir) {
                Ok(Some(session)) => {
                    self.sessions
                        .write()
                        .unwrap()
                        .insert(session.id().to_string(), session);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("skipping session at {}: {e:#}", dir.display()),
            }
        }
        Ok(())
    }

    fn restore_one(&self, dir: &Path) -> Result<Option<Arc<Session>>> {
        let meta_path = dir.join("meta.json");
        if !meta_path.is_file() {
            return Ok(None);
        }
        let meta: SessionMeta = serde_json::from_slice(&fs::read(&meta_path)?)?;
        let Some(ws) = self.workspace(&meta.workspace) else {
            tracing::warn!(
                "session {} belongs to workspace '{}', which is not registered; skipping",
                meta.id,
                meta.workspace
            );
            return Ok(None);
        };
        let log_path = dir.join("events.jsonl");
        let mut events = Vec::new();
        if log_path.is_file() {
            for line in fs::read_to_string(&log_path)?.lines() {
                // A torn final line from a crash ends the usable log.
                match serde_json::from_str::<Event>(line) {
                    Ok(e) if e.seq == events.len() as u64 + 1 => events.push(e),
                    _ => break,
                }
            }
        }
        let interrupted = events
            .last()
            .is_some_and(|e| !matches!(e.kind, EventKind::TurnFinished));
        let session = self.build(meta, ws.clone(), events, log_path);
        if interrupted {
            session.emit(EventKind::Error {
                message: "the daemon stopped while this turn was running".into(),
            });
            session.emit(EventKind::TurnFinished);
        }
        Ok(Some(session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StubModel;
    use crate::permissions::Mode;

    struct Fixture {
        _dirs: (tempfile::TempDir, tempfile::TempDir),
        data: PathBuf,
        ws: Workspace,
    }

    fn fixture() -> Fixture {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("hello.txt"), "hi there").unwrap();
        Fixture {
            data: data.path().to_path_buf(),
            ws: Workspace::new("demo", root.path()).unwrap(),
            _dirs: (data, root),
        }
    }

    fn manager(f: &Fixture, mode: Mode, timeout: Duration) -> SessionManager {
        SessionManager::open_with_timeout(
            &f.data,
            vec![f.ws.clone()],
            Policy::new(mode),
            Arc::new(StubModel),
            Arc::new(ToolRegistry::builtin()),
            timeout,
        )
        .unwrap()
    }

    async fn until_finished(rx: &mut broadcast::Receiver<Event>) -> Vec<EventKind> {
        let mut seen = Vec::new();
        loop {
            let e = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("turn did not finish")
                .unwrap();
            let done = matches!(e.kind, EventKind::TurnFinished);
            seen.push(e.kind);
            if done {
                return seen;
            }
        }
    }

    #[tokio::test]
    async fn read_runs_without_asking_and_log_survives_restart() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let s = m.create("demo").unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/read hello.txt".into()).unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(seen.iter().any(
            |k| matches!(k, EventKind::ToolResult { ok: true, output, .. } if output == "hi there")
        ));
        assert!(!seen
            .iter()
            .any(|k| matches!(k, EventKind::PermissionRequested { .. })));
        let id = s.id().to_string();
        let count = s.info().last_seq;
        drop((s, m));

        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let s = m.get(&id).expect("session restored");
        assert_eq!(s.info().last_seq, count);
        let (replay, _) = s.subscribe_from(1);
        assert_eq!(replay.len() as u64, count - 1);
        assert_eq!(replay[0].seq, 2);
    }

    #[tokio::test]
    async fn write_waits_for_approval() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let s = m.create("demo").unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write out.txt some text".into())
            .unwrap();
        assert_eq!(
            s.submit_user_message("again".into()),
            Err(SubmitError::Busy)
        );
        let call_id = loop {
            if let EventKind::PermissionRequested { call_id, .. } = rx.recv().await.unwrap().kind {
                break call_id;
            }
        };
        assert!(!f.ws.root().join("out.txt").exists());
        assert!(s.resolve_permission(&call_id, true));
        let seen = until_finished(&mut rx).await;
        assert!(seen
            .iter()
            .any(|k| matches!(k, EventKind::ToolResult { ok: true, .. })));
        assert_eq!(
            fs::read_to_string(f.ws.root().join("out.txt")).unwrap(),
            "some text"
        );
        assert!(!s.info().running);
    }

    #[tokio::test]
    async fn denial_timeout_and_plan_mode_all_block_the_write() {
        let f = fixture();

        // Explicit denial.
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let s = m.create("demo").unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        let call_id = loop {
            if let EventKind::PermissionRequested { call_id, .. } = rx.recv().await.unwrap().kind {
                break call_id;
            }
        };
        s.resolve_permission(&call_id, false);
        let seen = until_finished(&mut rx).await;
        assert!(seen
            .iter()
            .any(|k| matches!(k, EventKind::ToolResult { ok: false, .. })));

        // Nobody answers.
        let m = manager(&f, Mode::Ask, Duration::from_millis(50));
        let s = m.create("demo").unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(seen.iter().any(|k| matches!(
            k,
            EventKind::PermissionResolved { allowed: false, by, .. } if by == "timeout"
        )));

        // Plan mode never asks.
        let m = manager(&f, Mode::Plan, Duration::from_secs(5));
        let s = m.create("demo").unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(!seen
            .iter()
            .any(|k| matches!(k, EventKind::PermissionRequested { .. })));

        assert!(!f.ws.root().join("a.txt").exists());
    }

    #[tokio::test]
    async fn interrupt_denies_pending_request() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(30));
        let s = m.create("demo").unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        loop {
            if let EventKind::PermissionRequested { .. } = rx.recv().await.unwrap().kind {
                break;
            }
        }
        s.interrupt();
        let seen = until_finished(&mut rx).await;
        assert!(seen.iter().any(|k| matches!(
            k,
            EventKind::PermissionResolved { allowed: false, by, .. } if by == "interrupt"
        )));
        assert!(!f.ws.root().join("a.txt").exists());
        assert!(m.create("nope").is_err());
    }
}
