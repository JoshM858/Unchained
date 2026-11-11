//! The model boundary. A [`ModelAdapter`] sees the session history and
//! returns the next turn: some text, some tool requests, or both.
//!
//! Real adapters (OpenAI-compatible endpoints such as Ollama, llama.cpp,
//! vLLM; per-model profiles) are the next milestone. [`StubModel`] stands in
//! so the loop, permissions and UI can be exercised end to end without one.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use unchained_protocol::{Event, EventKind};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRequest {
    pub tool: String,
    pub input: Value,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelTurn {
    pub text: String,
    pub tool_calls: Vec<ToolRequest>,
}

#[async_trait]
pub trait ModelAdapter: Send + Sync {
    async fn next_turn(&self, history: &[Event]) -> Result<ModelTurn>;
}

/// Not a model. Maps slash commands in the user's message to tool calls:
/// `/ls [dir]`, `/read <file>`, `/write <file> <text>`.
pub struct StubModel;

const STUB_HELP: &str = "No model is configured yet; this is the built-in stub. \
It understands: /ls [dir], /read <file>, /write <file> <text>.";

#[async_trait]
impl ModelAdapter for StubModel {
    async fn next_turn(&self, history: &[Event]) -> Result<ModelTurn> {
        let last_user = history
            .iter()
            .rposition(|e| matches!(e.kind, EventKind::UserMessage { .. }));
        let Some(idx) = last_user else {
            return Ok(ModelTurn::default());
        };
        // A tool already ran for this message: the turn is complete.
        if history[idx..]
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolResult { .. }))
        {
            return Ok(ModelTurn::default());
        }
        let EventKind::UserMessage { text } = &history[idx].kind else {
            unreachable!()
        };
        let text = text.trim();
        let (cmd, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let rest = rest.trim();
        let call = |tool: &str, input: Value| ModelTurn {
            text: String::new(),
            tool_calls: vec![ToolRequest {
                tool: tool.into(),
                input,
            }],
        };
        Ok(match cmd {
            "/ls" => call(
                "list_dir",
                json!({ "path": if rest.is_empty() { "." } else { rest } }),
            ),
            "/read" if !rest.is_empty() => call("read_file", json!({ "path": rest })),
            "/write" if !rest.is_empty() => {
                let (path, content) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                call("write_file", json!({ "path": path, "content": content }))
            }
            _ => ModelTurn {
                text: STUB_HELP.into(),
                tool_calls: Vec::new(),
            },
        })
    }
}
