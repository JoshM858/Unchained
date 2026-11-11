//! Tools are the agent's hands. Each declares its [`Effect`] so the
//! permission engine can judge a call before it runs.
//!
//! The file tools act on the project's workspace. Scratch tools act on the
//! session's own scratch directory. Note tools act on the project's vault.
//! There is deliberately no shell tool yet: it lands together with the OS
//! sandbox, not before it.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::artifacts::ArtifactStore;
use crate::notes;
use crate::permissions::Effect;
use crate::workspace::Workspace;

/// Largest file `read_file` returns, and largest payload `write_file` accepts.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Tools that never need approval. They touch only session scratch space or
/// the artifact store, both owned by the daemon, so the permission mode does
/// not apply to them.
pub const ALWAYS_ALLOWED: [&str; 5] = [
    "scratch_write",
    "scratch_read",
    "scratch_list",
    "create_artifact",
    "search_notes",
];

/// Everything a tool may touch for one call.
pub struct ToolEnv<'a> {
    /// The project's workspace root.
    pub workspace: &'a Workspace,
    /// This session's scratch directory.
    pub scratch: &'a Workspace,
    /// The project's notes vault, when it has one.
    pub vault: Option<&'a Workspace>,
    /// The project's artifact store.
    pub artifacts: Option<&'a ArtifactStore>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn effect(&self) -> Effect;
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn builtin() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(ReadFile));
        r.register(Arc::new(ListDir));
        r.register(Arc::new(WriteFile));
        r.register(Arc::new(ScratchWrite));
        r.register(Arc::new(ScratchRead));
        r.register(Arc::new(ScratchList));
        r.register(Arc::new(PromoteScratch));
        r.register(Arc::new(CreateArtifact));
        r.register(Arc::new(SearchNotes));
        r.register(Arc::new(ReadNote));
        r.register(Arc::new(WriteNote));
        r
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.tools.keys().copied()
    }
}

fn str_field<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field '{key}'"))
}

/// Read a UTF-8 text file inside `ws`, within the size limit.
pub fn read_text(ws: &Workspace, rel: &str) -> Result<String> {
    let path = ws.resolve(rel)?;
    let meta = fs::metadata(&path).with_context(|| rel.to_string())?;
    if !meta.is_file() {
        bail!("{rel} is not a file");
    }
    if meta.len() > MAX_FILE_BYTES {
        bail!(
            "{rel} is {} bytes; the limit is {MAX_FILE_BYTES}",
            meta.len()
        );
    }
    let bytes = fs::read(&path).with_context(|| rel.to_string())?;
    String::from_utf8(bytes).map_err(|_| anyhow!("{rel} is not valid UTF-8"))
}

/// List a directory inside `ws`, one name per line, folders suffixed `/`.
pub fn list_text(ws: &Workspace, rel: &str) -> Result<String> {
    let path = ws.resolve(rel)?;
    let dir = fs::read_dir(&path).with_context(|| rel.to_string())?;
    let mut names = Vec::new();
    for entry in dir {
        let entry = entry?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            name.push('/');
        }
        names.push(name);
    }
    names.sort();
    Ok(names.join("\n"))
}

fn write_text(ws: &Workspace, rel: &str, content: &str) -> Result<usize> {
    if content.len() as u64 > MAX_FILE_BYTES {
        bail!(
            "content is {} bytes; the limit is {MAX_FILE_BYTES}",
            content.len()
        );
    }
    let path = ws.prepare(rel)?;
    if path.is_dir() {
        bail!("{rel} is a directory");
    }
    fs::write(&path, content).with_context(|| rel.to_string())?;
    Ok(content.len())
}

fn vault<'a>(env: &ToolEnv<'a>) -> Result<&'a Workspace> {
    env.vault
        .ok_or_else(|| anyhow!("this project has no notes vault"))
}

fn artifacts<'a>(env: &ToolEnv<'a>) -> Result<&'a ArtifactStore> {
    env.artifacts
        .ok_or_else(|| anyhow!("artifacts are not available here"))
}

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a UTF-8 text file in the project. Input: {\"path\": \"relative/path\"}"
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        read_text(env.workspace, str_field(input, "path")?)
    }
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn description(&self) -> &'static str {
        "List a project directory. Input: {\"path\": \"relative/dir\"} (default \".\")"
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let rel = input.get("path").and_then(Value::as_str).unwrap_or(".");
        list_text(env.workspace, rel)
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a text file in the project. Input: {\"path\": \"relative/path\", \"content\": \"...\"}"
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let rel = str_field(input, "path")?;
        let n = write_text(env.workspace, rel, str_field(input, "content")?)?;
        Ok(format!("wrote {n} bytes to {rel}"))
    }
}

pub struct ScratchWrite;

#[async_trait]
impl Tool for ScratchWrite {
    fn name(&self) -> &'static str {
        "scratch_write"
    }
    fn description(&self) -> &'static str {
        "Write a temporary file in this session's scratch space (outside the project; no approval needed). Input: {\"path\": \"name.txt\", \"content\": \"...\"}"
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let rel = str_field(input, "path")?;
        let n = write_text(env.scratch, rel, str_field(input, "content")?)?;
        Ok(format!("wrote {n} bytes to scratch/{rel}"))
    }
}

pub struct ScratchRead;

#[async_trait]
impl Tool for ScratchRead {
    fn name(&self) -> &'static str {
        "scratch_read"
    }
    fn description(&self) -> &'static str {
        "Read a file from this session's scratch space. Input: {\"path\": \"name.txt\"}"
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        read_text(env.scratch, str_field(input, "path")?)
    }
}

pub struct ScratchList;

#[async_trait]
impl Tool for ScratchList {
    fn name(&self) -> &'static str {
        "scratch_list"
    }
    fn description(&self) -> &'static str {
        "List this session's scratch space. Input: {\"path\": \"dir\"} (default \".\")"
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let rel = input.get("path").and_then(Value::as_str).unwrap_or(".");
        list_text(env.scratch, rel)
    }
}

pub struct PromoteScratch;

#[async_trait]
impl Tool for PromoteScratch {
    fn name(&self) -> &'static str {
        "promote_scratch"
    }
    fn description(&self) -> &'static str {
        "Copy a scratch file into the project. Input: {\"from\": \"scratch name\", \"to\": \"project/relative/path\"}. Needs approval like any write."
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let content = read_text(env.scratch, str_field(input, "from")?)?;
        let to = str_field(input, "to")?;
        let n = write_text(env.workspace, to, &content)?;
        Ok(format!("promoted {n} bytes to {to}"))
    }
}

pub struct CreateArtifact;

#[async_trait]
impl Tool for CreateArtifact {
    fn name(&self) -> &'static str {
        "create_artifact"
    }
    fn description(&self) -> &'static str {
        "Save a self-contained result as a versioned artifact. Input: {\"name\": \"lowercase-name\", \"kind\": \"html|svg|markdown|code\", \"content\": \"...\"}. Saving an existing name adds a version."
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let meta = artifacts(env)?.create(
            str_field(input, "name")?,
            str_field(input, "kind")?,
            str_field(input, "content")?,
        )?;
        let version = meta.versions.last().map_or(1, |v| v.version);
        Ok(format!("saved artifact {} as version {version}", meta.name))
    }
}

pub struct SearchNotes;

#[async_trait]
impl Tool for SearchNotes {
    fn name(&self) -> &'static str {
        "search_notes"
    }
    fn description(&self) -> &'static str {
        "Search the notes vault. Input: {\"query\": \"text\"}. Returns matching note paths with line numbers."
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let hits = notes::search(vault(env)?, str_field(input, "query")?, 20)?;
        if hits.is_empty() {
            return Ok("no matches".into());
        }
        Ok(hits
            .iter()
            .map(|h| format!("{}:{}: {}", h.path, h.line, h.snippet))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

pub struct ReadNote;

#[async_trait]
impl Tool for ReadNote {
    fn name(&self) -> &'static str {
        "read_note"
    }
    fn description(&self) -> &'static str {
        "Read a note from the vault. Input: {\"path\": \"Folder/Note.md\"}"
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        notes::read_note(vault(env)?, str_field(input, "path")?)
    }
}

pub struct WriteNote;

#[async_trait]
impl Tool for WriteNote {
    fn name(&self) -> &'static str {
        "write_note"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a note in the vault. Input: {\"path\": \"Folder/Note.md\", \"content\": \"...\"}. Subject to the project's vault mode."
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    async fn run(&self, env: &ToolEnv<'_>, input: &Value) -> Result<String> {
        let rel = str_field(input, "path")?;
        let n = notes::write_note(vault(env)?, rel, str_field(input, "content")?)?;
        Ok(format!("wrote {n} bytes to {rel}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env<'a>(ws: &'a Workspace, scratch: &'a Workspace) -> ToolEnv<'a> {
        ToolEnv {
            workspace: ws,
            scratch,
            vault: None,
            artifacts: None,
        }
    }

    #[tokio::test]
    async fn file_tools_work_and_stay_inside() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new("t", dir.path()).unwrap();
        let scratch = Workspace::new("s", dir.path()).unwrap();
        let reg = ToolRegistry::builtin();
        let e = env(&ws, &scratch);

        let out = reg
            .get("write_file")
            .unwrap()
            .run(&e, &json!({"path": "a.txt", "content": "hello"}))
            .await
            .unwrap();
        assert!(out.contains("5 bytes"));
        let read = reg.get("read_file").unwrap();
        assert_eq!(
            read.run(&e, &json!({"path": "a.txt"})).await.unwrap(),
            "hello"
        );
        let ls = reg
            .get("list_dir")
            .unwrap()
            .run(&e, &json!({}))
            .await
            .unwrap();
        assert_eq!(ls, "a.txt");
        assert!(read.run(&e, &json!({"path": "../x"})).await.is_err());
        assert!(reg
            .get("write_file")
            .unwrap()
            .run(&e, &json!({"path": "/tmp/x", "content": ""}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn note_and_artifact_tools_need_their_stores() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new("t", dir.path()).unwrap();
        let reg = ToolRegistry::builtin();
        let e = env(&ws, &ws);
        assert!(reg
            .get("search_notes")
            .unwrap()
            .run(&e, &json!({"query": "x"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("no notes vault"));
        assert!(reg
            .get("create_artifact")
            .unwrap()
            .run(&e, &json!({"name": "a", "kind": "code", "content": "x"}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn promote_copies_scratch_into_the_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("project")).unwrap();
        std::fs::create_dir_all(dir.path().join("scratch")).unwrap();
        let ws = Workspace::new("p", dir.path().join("project")).unwrap();
        let scratch = Workspace::new("s", dir.path().join("scratch")).unwrap();
        let reg = ToolRegistry::builtin();
        let e = env(&ws, &scratch);
        reg.get("scratch_write")
            .unwrap()
            .run(&e, &json!({"path": "draft.md", "content": "# draft"}))
            .await
            .unwrap();
        reg.get("promote_scratch")
            .unwrap()
            .run(&e, &json!({"from": "draft.md", "to": "docs/draft.md"}))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("project/docs/draft.md")).unwrap(),
            "# draft"
        );
    }
}
