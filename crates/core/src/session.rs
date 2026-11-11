//! Sessions and the agent loop.
//!
//! A [`Session`] owns an append-only event log, mirrored to
//! `<data_dir>/sessions/<id>/events.jsonl`. The daemon, not any client, runs
//! the loop: a turn keeps going when every browser is closed, and a client
//! that reconnects replays what it missed.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot};
use unchained_protocol::{
    ArchiveInfo, ArtifactMeta, CreateProject, Event, EventKind, FileEntry, NoteEntry, Project,
    SearchHit, SessionInfo, UpdateProject, VaultMode, WorkspaceInfo,
};

use crate::artifacts::ArtifactStore;
use crate::model::ModelAdapter;
use crate::notes;
use crate::permissions::{Effect, Mode, Policy, Rule, Verdict};
use crate::project::{load_instructions, now_ms, ProjectHandle, ProjectStore};
use crate::scratch;
use crate::tools::{self, ToolEnv, ToolRegistry, ALWAYS_ALLOWED};
use crate::workspace::Workspace;

/// Model round-trips allowed in one turn before the loop stops itself.
const MAX_STEPS: usize = 32;
/// An unanswered permission request is denied after this long.
const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BROADCAST_CAPACITY: usize = 1024;
/// Most entries a single directory listing returns.
const MAX_DIR_ENTRIES: usize = 1000;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SubmitError {
    #[error("a turn is already running in this session")]
    Busy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionMeta {
    id: String,
    /// Empty only in sessions written before projects existed; those are adopted on open.
    #[serde(default)]
    project: String,
    workspace: String,
    created_ms: u64,
}

fn verdict_for_vault(mode: VaultMode) -> Verdict {
    match mode {
        VaultMode::ReadOnly => Verdict::Deny,
        VaultMode::ReadWriteAsk => Verdict::Ask,
        VaultMode::ReadWrite => Verdict::Allow,
    }
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
    project: ProjectHandle,
    scratch: Workspace,
    artifacts: ArtifactStore,
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
            project: self.meta.project.clone(),
            workspace: self.meta.workspace.clone(),
            created_ms: self.meta.created_ms,
            running: self.running.load(Ordering::SeqCst),
            last_seq: self.events.lock().unwrap().len() as u64,
        }
    }

    /// Permission requests currently waiting for an answer.
    pub fn pending_approvals(&self) -> usize {
        self.pending.lock().unwrap().len()
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

    fn vault(&self) -> Option<Workspace> {
        let path = self.project.read().unwrap().vault.clone()?;
        Workspace::new("vault", path).ok()
    }

    async fn run_turn(&self) {
        // Read fresh each turn, so an edit to AGENTS.md applies to the next message.
        let instructions = load_instructions(&self.workspace);
        for _ in 0..MAX_STEPS {
            if self.cancel.load(Ordering::SeqCst) {
                self.emit(EventKind::Error {
                    message: "interrupted".into(),
                });
                return;
            }
            let history = self.events.lock().unwrap().clone();
            let turn = match self
                .deps
                .model
                .next_turn(&history, instructions.as_ref())
                .await
            {
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
        let vault = self.vault();
        let verdict = self.verdict(&tool_name, tool.effect());
        let allowed = match verdict {
            Verdict::Allow => true,
            Verdict::Deny => false,
            Verdict::Ask => self.ask(&call_id, &tool_name, &input).await,
        };
        if !allowed {
            self.emit(result(false, "permission denied".into()));
            return;
        }
        let env = ToolEnv {
            workspace: &self.workspace,
            scratch: &self.scratch,
            vault: vault.as_ref(),
            artifacts: Some(&self.artifacts),
        };
        match tool.run(&env, &input).await {
            Ok(output) => self.emit(result(true, output)),
            Err(e) => self.emit(result(false, format!("{e:#}"))),
        }
    }

    /// The policy verdict for a call. Note writes are additionally bound by
    /// the project's vault mode; the stricter answer wins.
    fn verdict(&self, tool: &str, effect: Effect) -> Verdict {
        let policy = self.deps.policy.decide(tool, effect);
        if tool == "write_note" {
            let mode = self.project.read().unwrap().vault_mode;
            return policy.stricter(verdict_for_vault(mode));
        }
        policy
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

/// Owns every session, every project and the workspaces they may run in.
pub struct SessionManager {
    data_dir: PathBuf,
    workspaces: Vec<Workspace>,
    deps: Arc<Deps>,
    projects: ProjectStore,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

impl SessionManager {
    /// Open `data_dir`, restoring any sessions and projects persisted there.
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
        mut policy: Policy,
        model: Arc<dyn ModelAdapter>,
        tools: Arc<ToolRegistry>,
        approval_timeout: Duration,
    ) -> Result<Self> {
        let data_dir = data_dir.into();
        fs::create_dir_all(data_dir.join("sessions"))
            .with_context(|| format!("creating {}", data_dir.display()))?;
        fs::create_dir_all(scratch::scratch_root(&data_dir))?;
        // Appended after the caller's rules, so they cannot be overridden
        // by accident: these tools only touch daemon-owned space.
        for tool in ALWAYS_ALLOWED {
            policy.rules.push(Rule {
                tool: Some(tool.to_string()),
                effect: None,
                verdict: Verdict::Allow,
            });
        }
        let projects = ProjectStore::open(&data_dir)?;
        let manager = Self {
            data_dir,
            workspaces,
            deps: Arc::new(Deps {
                model,
                tools,
                policy,
                approval_timeout,
            }),
            projects,
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

    pub fn mode(&self) -> Mode {
        self.deps.policy.mode
    }

    /// Permission requests waiting across every session.
    pub fn pending_approvals(&self) -> usize {
        self.sessions
            .read()
            .unwrap()
            .values()
            .map(|s| s.pending_approvals())
            .sum()
    }

    /// List a directory inside a workspace: directories first, then by name.
    pub async fn list_files(&self, workspace: &str, rel: &str) -> Result<Vec<FileEntry>> {
        let ws = self
            .workspace(workspace)
            .ok_or_else(|| anyhow!("unknown workspace '{workspace}'"))?;
        list_entries(ws, rel)
    }

    /// Read a text file inside a workspace, with the same limits as the
    /// agent's `read_file` tool.
    pub async fn read_file(&self, workspace: &str, rel: &str) -> Result<String> {
        let ws = self
            .workspace(workspace)
            .ok_or_else(|| anyhow!("unknown workspace '{workspace}'"))?;
        tools::read_text(ws, rel)
    }

    // ---- projects ----

    pub fn projects(&self) -> Vec<Project> {
        self.projects.list()
    }

    pub fn project(&self, id: &str) -> Option<Project> {
        self.projects.get(id).map(|h| h.read().unwrap().clone())
    }

    pub fn create_project(&self, input: CreateProject) -> Result<Project> {
        self.projects.create(input, &self.workspaces)
    }

    pub fn update_project(&self, id: &str, input: UpdateProject) -> Result<Project> {
        self.projects.update(id, input)
    }

    fn project_handle(&self, id: &str) -> Result<ProjectHandle> {
        self.projects
            .get(id)
            .ok_or_else(|| anyhow!("no project '{id}'"))
    }

    fn artifacts_dir(&self, project: &str) -> PathBuf {
        self.data_dir
            .join("projects")
            .join(project)
            .join("artifacts")
    }

    /// The project's instructions file, as the agent would read it.
    pub fn instructions(&self, project: &str) -> Result<Option<crate::project::Instructions>> {
        let handle = self.project_handle(project)?;
        let ws_name = handle.read().unwrap().workspace.clone();
        let ws = self
            .workspace(&ws_name)
            .ok_or_else(|| anyhow!("unknown workspace '{ws_name}'"))?;
        Ok(load_instructions(ws))
    }

    pub fn artifacts(&self, project: &str) -> Result<Vec<ArtifactMeta>> {
        self.project_handle(project)?;
        ArtifactStore::new(self.artifacts_dir(project)).list()
    }

    pub fn artifact(
        &self,
        project: &str,
        name: &str,
        version: Option<u32>,
    ) -> Result<(ArtifactMeta, u32, String)> {
        self.project_handle(project)?;
        ArtifactStore::new(self.artifacts_dir(project)).read(name, version)
    }

    fn vault(&self, project: &str) -> Result<Workspace> {
        let p = self
            .project(project)
            .ok_or_else(|| anyhow!("no project '{project}'"))?;
        let path = p
            .vault
            .ok_or_else(|| anyhow!("this project has no notes vault"))?;
        Ok(Workspace::new("vault", path)?)
    }

    pub fn notes(&self, project: &str) -> Result<Vec<NoteEntry>> {
        notes::list_notes(&self.vault(project)?)
    }

    pub fn note(&self, project: &str, path: &str) -> Result<String> {
        notes::read_note(&self.vault(project)?, path)
    }

    pub fn backlinks(&self, project: &str, path: &str) -> Result<Vec<String>> {
        notes::backlinks(&self.vault(project)?, path)
    }

    pub fn search_notes(&self, project: &str, query: &str) -> Result<Vec<SearchHit>> {
        notes::search(&self.vault(project)?, query, 50)
    }

    // ---- sessions ----

    /// Start a session in a project.
    pub fn create(&self, project: &str) -> Result<Arc<Session>> {
        let p = self
            .project(project)
            .ok_or_else(|| anyhow!("no project '{project}'"))?;
        let ws = self
            .workspace(&p.workspace)
            .ok_or_else(|| anyhow!("unknown workspace '{}'", p.workspace))?
            .clone();
        let meta = SessionMeta {
            id: uuid::Uuid::new_v4().simple().to_string(),
            project: p.id.clone(),
            workspace: ws.name().to_string(),
            created_ms: now_ms(),
        };
        let dir = self.data_dir.join("sessions").join(&meta.id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
        let session = self.build(meta, ws, Vec::new(), dir.join("events.jsonl"))?;
        self.sessions
            .write()
            .unwrap()
            .insert(session.id().to_string(), Arc::clone(&session));
        Ok(session)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.read().unwrap().get(id).cloned()
    }

    /// Sessions, newest first. With `project`, only that project's.
    pub fn list(&self, project: Option<&str>) -> Vec<SessionInfo> {
        let mut infos: Vec<_> = self
            .sessions
            .read()
            .unwrap()
            .values()
            .map(|s| s.info())
            .filter(|s| project.is_none_or(|p| s.project == p))
            .collect();
        infos.sort_by_key(|s| std::cmp::Reverse(s.created_ms));
        infos
    }

    /// Delete a session and its scratch space. Refused while a turn runs.
    pub fn delete(&self, id: &str) -> Result<()> {
        let session = self.get(id).ok_or_else(|| anyhow!("no session '{id}'"))?;
        if session.info().running {
            bail!("the session is running; interrupt it first");
        }
        self.sessions.write().unwrap().remove(id);
        fs::remove_dir_all(self.data_dir.join("sessions").join(id))?;
        let scratch_dir = scratch::scratch_root(&self.data_dir).join(id);
        if scratch_dir.exists() {
            fs::remove_dir_all(&scratch_dir)?;
        }
        Ok(())
    }

    fn scratch_ws(&self, session: &str) -> Result<Workspace> {
        self.get(session)
            .ok_or_else(|| anyhow!("no session '{session}'"))?;
        let dir = scratch::scratch_root(&self.data_dir).join(session);
        fs::create_dir_all(&dir)?;
        Ok(Workspace::new("scratch", dir)?)
    }

    pub fn scratch_list(&self, session: &str, rel: &str) -> Result<Vec<FileEntry>> {
        list_entries(&self.scratch_ws(session)?, rel)
    }

    pub fn scratch_read(&self, session: &str, rel: &str) -> Result<String> {
        tools::read_text(&self.scratch_ws(session)?, rel)
    }

    /// Archive idle scratch space (see [`scratch::sweep`]). Running sessions are left alone.
    pub fn sweep_scratch(&self, now: SystemTime) -> Result<Vec<ArchiveInfo>> {
        let running: HashSet<String> = self
            .sessions
            .read()
            .unwrap()
            .values()
            .filter(|s| s.info().running)
            .map(|s| s.id().to_string())
            .collect();
        scratch::sweep(&self.data_dir, now, scratch::MAX_AGE, |s| {
            running.contains(s)
        })
    }

    pub fn archives(&self) -> Result<Vec<ArchiveInfo>> {
        scratch::list_archives(&self.data_dir)
    }

    /// Restore an archive into its session's scratch directory.
    pub fn restore_archive(&self, name: &str) -> Result<String> {
        scratch::restore(&self.data_dir, name)
    }

    fn build(
        &self,
        meta: SessionMeta,
        workspace: Workspace,
        events: Vec<Event>,
        log_path: PathBuf,
    ) -> Result<Arc<Session>> {
        let project = self.project_handle(&meta.project)?;
        let scratch_dir = scratch::scratch_root(&self.data_dir).join(&meta.id);
        fs::create_dir_all(&scratch_dir)?;
        let scratch = Workspace::new("scratch", &scratch_dir)?;
        let artifacts = ArtifactStore::new(self.artifacts_dir(&meta.project));
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Arc::new(Session {
            meta,
            workspace,
            project,
            scratch,
            artifacts,
            deps: Arc::clone(&self.deps),
            events: Mutex::new(events),
            tx,
            pending: Mutex::new(HashMap::new()),
            running: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            log_path,
        }))
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
        let mut meta: SessionMeta = serde_json::from_slice(&fs::read(&meta_path)?)?;
        let Some(ws) = self.workspace(&meta.workspace).cloned() else {
            tracing::warn!(
                "session {} belongs to workspace '{}', which is not registered; skipping",
                meta.id,
                meta.workspace
            );
            return Ok(None);
        };
        if meta.project.is_empty() {
            // Written before projects existed: adopt it into one named after its workspace.
            meta.project = self.projects.ensure_for_workspace(&meta.workspace)?;
            fs::write(&meta_path, serde_json::to_vec_pretty(&meta)?)?;
        }
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
        let session = self.build(meta, ws, events, log_path)?;
        if interrupted {
            session.emit(EventKind::Error {
                message: "the daemon stopped while this turn was running".into(),
            });
            session.emit(EventKind::TurnFinished);
        }
        Ok(Some(session))
    }
}

/// Directory listing used by the file browser: folders first, then by name.
fn list_entries(ws: &Workspace, rel: &str) -> Result<Vec<FileEntry>> {
    let path = ws.resolve(rel)?;
    let dir = std::fs::read_dir(&path).with_context(|| rel.to_string())?;
    let mut entries = Vec::new();
    for entry in dir {
        let entry = entry?;
        if entries.len() >= MAX_DIR_ENTRIES {
            break;
        }
        // Symlinks are listed as files; opening one still goes through
        // `Workspace::resolve`, which refuses targets outside the root.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        entries.push(FileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            dir: meta.is_dir(),
            size: if meta.is_file() { meta.len() } else { 0 },
        });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StubModel;
    use crate::permissions::Mode;

    struct Fixture {
        _dirs: (tempfile::TempDir, tempfile::TempDir),
        data: PathBuf,
        root: PathBuf,
        ws: Workspace,
    }

    fn fixture() -> Fixture {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("hello.txt"), "hi there").unwrap();
        Fixture {
            data: data.path().to_path_buf(),
            root: root.path().to_path_buf(),
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

    fn project(m: &SessionManager, vault: Option<&Path>, mode: VaultMode) -> String {
        m.create_project(CreateProject {
            name: "p".into(),
            workspace: "demo".into(),
            vault: vault.map(|v| v.display().to_string()),
            vault_mode: Some(mode),
        })
        .unwrap()
        .id
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

    async fn wait_for_request(rx: &mut broadcast::Receiver<Event>) -> String {
        loop {
            if let EventKind::PermissionRequested { call_id, .. } = rx.recv().await.unwrap().kind {
                return call_id;
            }
        }
    }

    #[tokio::test]
    async fn read_runs_without_asking_and_log_survives_restart() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let pid = project(&m, None, VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
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
        assert_eq!(s.info().project, pid);
        let (replay, _) = s.subscribe_from(1);
        assert_eq!(replay.len() as u64, count - 1);
        assert_eq!(replay[0].seq, 2);
    }

    #[tokio::test]
    async fn write_waits_for_approval() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let pid = project(&m, None, VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write out.txt some text".into())
            .unwrap();
        assert_eq!(
            s.submit_user_message("again".into()),
            Err(SubmitError::Busy)
        );
        let call_id = wait_for_request(&mut rx).await;
        assert!(!f.root.join("out.txt").exists());
        assert!(s.resolve_permission(&call_id, true));
        let seen = until_finished(&mut rx).await;
        assert!(seen
            .iter()
            .any(|k| matches!(k, EventKind::ToolResult { ok: true, .. })));
        assert_eq!(
            fs::read_to_string(f.root.join("out.txt")).unwrap(),
            "some text"
        );
        assert!(!s.info().running);
    }

    #[tokio::test]
    async fn denial_timeout_and_plan_mode_all_block_the_write() {
        let f = fixture();

        // Explicit denial.
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let pid = project(&m, None, VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        let call_id = wait_for_request(&mut rx).await;
        s.resolve_permission(&call_id, false);
        let seen = until_finished(&mut rx).await;
        assert!(seen
            .iter()
            .any(|k| matches!(k, EventKind::ToolResult { ok: false, .. })));

        // Nobody answers.
        let m = manager(&f, Mode::Ask, Duration::from_millis(50));
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(seen.iter().any(|k| matches!(
            k,
            EventKind::PermissionResolved { allowed: false, by, .. } if by == "timeout"
        )));

        // Plan mode never asks.
        let m = manager(&f, Mode::Plan, Duration::from_secs(5));
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(!seen
            .iter()
            .any(|k| matches!(k, EventKind::PermissionRequested { .. })));

        assert!(!f.root.join("a.txt").exists());
    }

    #[tokio::test]
    async fn interrupt_denies_pending_request() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(30));
        let pid = project(&m, None, VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/write a.txt x".into()).unwrap();
        wait_for_request(&mut rx).await;
        s.interrupt();
        let seen = until_finished(&mut rx).await;
        assert!(seen.iter().any(|k| matches!(
            k,
            EventKind::PermissionResolved { allowed: false, by, .. } if by == "interrupt"
        )));
        assert!(!f.root.join("a.txt").exists());
        assert!(m.create("nope").is_err());
    }

    #[tokio::test]
    async fn vault_mode_is_enforced_by_the_daemon() {
        let f = fixture();
        let vault = tempfile::tempdir().unwrap();
        let note = vault.path().join("Inbox/hello-there.md");

        // read-only: refused outright, no approval is ever requested.
        let m = manager(&f, Mode::AutoEdit, Duration::from_secs(5));
        let pid = project(&m, Some(vault.path()), VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/note hello there".into()).unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(!seen
            .iter()
            .any(|k| matches!(k, EventKind::PermissionRequested { .. })));
        assert!(seen
            .iter()
            .any(|k| matches!(k, EventKind::ToolResult { ok: false, .. })));
        assert!(!note.exists());

        // read-write-ask: asks, then writes once allowed.
        let pid = m
            .update_project(
                &pid,
                UpdateProject {
                    vault: None,
                    vault_mode: Some(VaultMode::ReadWriteAsk),
                },
            )
            .unwrap()
            .id;
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/note hello there".into()).unwrap();
        let call_id = wait_for_request(&mut rx).await;
        assert!(!note.exists());
        s.resolve_permission(&call_id, true);
        until_finished(&mut rx).await;
        assert!(note.exists());

        // read-write under auto-edit: writes without asking.
        let pid = m
            .update_project(
                &pid,
                UpdateProject {
                    vault: None,
                    vault_mode: Some(VaultMode::ReadWrite),
                },
            )
            .unwrap()
            .id;
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/note second thought".into())
            .unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(!seen
            .iter()
            .any(|k| matches!(k, EventKind::PermissionRequested { .. })));
        assert!(vault.path().join("Inbox/second-thought.md").exists());
    }

    #[tokio::test]
    async fn scratch_and_artifacts_need_no_approval_even_in_plan_mode() {
        let f = fixture();
        let m = manager(&f, Mode::Plan, Duration::from_secs(5));
        let pid = project(&m, None, VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/scratch draft.md # hi".into())
            .unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(!seen
            .iter()
            .any(|k| matches!(k, EventKind::PermissionRequested { .. })));
        assert!(seen
            .iter()
            .any(|k| matches!(k, EventKind::ToolResult { ok: true, .. })));
        assert_eq!(m.scratch_read(s.id(), "draft.md").unwrap(), "# hi");

        s.submit_user_message("/artifact report markdown # Report".into())
            .unwrap();
        until_finished(&mut rx).await;
        let (meta, version, text) = m.artifact(&pid, "report", None).unwrap();
        assert_eq!(
            (meta.kind.as_str(), version, text.as_str()),
            ("markdown", 1, "# Report")
        );
        assert!(!f.root.join("report").exists());
    }

    #[tokio::test]
    async fn project_instructions_reach_the_model() {
        let f = fixture();
        std::fs::write(f.root.join("AGENTS.md"), "Use tabs.").unwrap();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let pid = project(&m, None, VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
        let (_, mut rx) = s.subscribe_from(0);
        s.submit_user_message("/context".into()).unwrap();
        let seen = until_finished(&mut rx).await;
        assert!(seen.iter().any(|k| matches!(
            k,
            EventKind::AssistantMessage { text } if text.contains("AGENTS.md")
        )));
    }

    #[tokio::test]
    async fn sessions_from_before_projects_are_adopted() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let s = m.create(&project(&m, None, VaultMode::ReadOnly)).unwrap();
        let id = s.id().to_string();
        drop((s, m));
        // Strip the project from the session, and remove the projects file,
        // as an old daemon would have left them.
        let meta_path = f.data.join("sessions").join(&id).join("meta.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta.as_object_mut().unwrap().remove("project");
        fs::write(&meta_path, meta.to_string()).unwrap();
        fs::remove_file(f.data.join("projects.json")).unwrap();

        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let adopted = m.get(&id).expect("adopted, not dropped").info().project;
        assert!(!adopted.is_empty());
        assert_eq!(m.project(&adopted).unwrap().workspace, "demo");
    }

    #[tokio::test]
    async fn delete_removes_the_session_and_its_scratch() {
        let f = fixture();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let pid = project(&m, None, VaultMode::ReadOnly);
        let s = m.create(&pid).unwrap();
        let id = s.id().to_string();
        m.scratch_list(&id, ".").unwrap();
        fs::write(scratch::scratch_root(&f.data).join(&id).join("x.txt"), "x").unwrap();
        m.delete(&id).unwrap();
        assert!(m.get(&id).is_none());
        assert!(!f.data.join("sessions").join(&id).exists());
        assert!(!scratch::scratch_root(&f.data).join(&id).exists());
    }

    #[tokio::test]
    async fn browsing_is_confined_to_registered_workspaces() {
        let f = fixture();
        fs::create_dir(f.root.join("src")).unwrap();
        let m = manager(&f, Mode::Ask, Duration::from_secs(5));
        let names: Vec<_> = m
            .list_files("demo", ".")
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.dir, e.size))
            .collect();
        assert_eq!(
            names,
            vec![("src".into(), true, 0), ("hello.txt".into(), false, 8)]
        );
        assert_eq!(m.read_file("demo", "hello.txt").await.unwrap(), "hi there");
        assert!(m.list_files("demo", "..").await.is_err());
        assert!(m.read_file("demo", "/etc/passwd").await.is_err());
        assert!(m.list_files("other", ".").await.is_err());
    }
}
