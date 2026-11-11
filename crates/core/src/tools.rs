//! Tools are the agent's hands. Each declares its [`Effect`] so the
//! permission engine can judge a call before it runs.
//!
//! The scaffold ships three file tools. There is deliberately no shell tool
//! yet: it lands together with the OS sandbox, not before it.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::permissions::Effect;
use crate::workspace::Workspace;

/// Largest file `read_file` returns, and largest payload `write_file` accepts.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn effect(&self) -> Effect;
    async fn run(&self, workspace: &Workspace, input: &Value) -> Result<String>;
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

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a UTF-8 text file. Input: {\"path\": \"relative/path\"}"
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, workspace: &Workspace, input: &Value) -> Result<String> {
        let rel = str_field(input, "path")?;
        let path = workspace.resolve(rel)?;
        let meta = tokio::fs::metadata(&path)
            .await
            .with_context(|| rel.to_string())?;
        if !meta.is_file() {
            bail!("{rel} is not a file");
        }
        if meta.len() > MAX_FILE_BYTES {
            bail!(
                "{rel} is {} bytes; the limit is {MAX_FILE_BYTES}",
                meta.len()
            );
        }
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| rel.to_string())?;
        String::from_utf8(bytes).map_err(|_| anyhow!("{rel} is not valid UTF-8"))
    }
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn description(&self) -> &'static str {
        "List a directory. Input: {\"path\": \"relative/dir\"} (default \".\")"
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    async fn run(&self, workspace: &Workspace, input: &Value) -> Result<String> {
        let rel = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let path = workspace.resolve(rel)?;
        let mut dir = tokio::fs::read_dir(&path)
            .await
            .with_context(|| rel.to_string())?;
        let mut names = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await?.is_dir() {
                name.push('/');
            }
            names.push(name);
        }
        names.sort();
        Ok(names.join("\n"))
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a text file. Input: {\"path\": \"relative/path\", \"content\": \"...\"}"
    }
    fn effect(&self) -> Effect {
        Effect::Write
    }
    async fn run(&self, workspace: &Workspace, input: &Value) -> Result<String> {
        let rel = str_field(input, "path")?;
        let content = str_field(input, "content")?;
        if content.len() as u64 > MAX_FILE_BYTES {
            bail!(
                "content is {} bytes; the limit is {MAX_FILE_BYTES}",
                content.len()
            );
        }
        let path = workspace.resolve(rel)?;
        if path.is_dir() {
            bail!("{rel} is a directory");
        }
        tokio::fs::write(&path, content)
            .await
            .with_context(|| rel.to_string())?;
        Ok(format!("wrote {} bytes to {rel}", content.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn file_tools_work_and_stay_inside() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new("t", dir.path()).unwrap();
        let reg = ToolRegistry::builtin();

        let out = reg
            .get("write_file")
            .unwrap()
            .run(&ws, &json!({"path": "a.txt", "content": "hello"}))
            .await
            .unwrap();
        assert!(out.contains("5 bytes"));
        let read = reg.get("read_file").unwrap();
        assert_eq!(
            read.run(&ws, &json!({"path": "a.txt"})).await.unwrap(),
            "hello"
        );
        let ls = reg
            .get("list_dir")
            .unwrap()
            .run(&ws, &json!({}))
            .await
            .unwrap();
        assert_eq!(ls, "a.txt");
        assert!(read.run(&ws, &json!({"path": "../x"})).await.is_err());
        assert!(reg
            .get("write_file")
            .unwrap()
            .run(&ws, &json!({"path": "/tmp/x", "content": ""}))
            .await
            .is_err());
    }
}
