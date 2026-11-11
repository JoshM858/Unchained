//! The model boundary. A [`ModelAdapter`] sees the session history and the
//! project's instructions, and returns the next turn: some text, some tool
//! requests, or both.
//!
//! Real adapters (OpenAI-compatible endpoints such as Ollama, llama.cpp,
//! vLLM; per-model profiles) are the next milestone. [`StubModel`] stands in
//! so the loop, permissions and UI can be exercised end to end without one.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use unchained_protocol::{Event, EventKind};

use crate::project::Instructions;

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
    async fn next_turn(
        &self,
        history: &[Event],
        instructions: Option<&Instructions>,
    ) -> Result<ModelTurn>;
}

/// Not a model. Maps slash commands in the user's message to tool calls.
pub struct StubModel;

const STUB_HELP: &str = "No model is configured yet; this is the built-in stub. It understands: \
/ls [dir], /read <file>, /write <file> <text>, /scratch <name> <text>, /scratch-ls [dir], \
/promote <scratch name> <project path>, /artifact <name> <html|svg|markdown|code> <content>, \
/note <text>, /find <words>, /context.";

fn slug(text: &str) -> String {
    let mut out = String::new();
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if out.len() + word.len() > 40 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word.to_lowercase());
    }
    if out.is_empty() {
        "note".into()
    } else {
        out
    }
}

#[async_trait]
impl ModelAdapter for StubModel {
    async fn next_turn(
        &self,
        history: &[Event],
        instructions: Option<&Instructions>,
    ) -> Result<ModelTurn> {
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
        let say = |text: String| ModelTurn {
            text,
            tool_calls: Vec::new(),
        };
        let (first, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let after = after.trim();
        Ok(match cmd {
            "/ls" => call(
                "list_dir",
                json!({ "path": if rest.is_empty() { "." } else { rest } }),
            ),
            "/read" if !rest.is_empty() => call("read_file", json!({ "path": rest })),
            "/write" if !rest.is_empty() => {
                call("write_file", json!({ "path": first, "content": after }))
            }
            "/scratch" if !after.is_empty() => {
                call("scratch_write", json!({ "path": first, "content": after }))
            }
            "/scratch-ls" => call(
                "scratch_list",
                json!({ "path": if rest.is_empty() { "." } else { rest } }),
            ),
            "/promote" if !after.is_empty() => {
                call("promote_scratch", json!({ "from": first, "to": after }))
            }
            "/artifact" if !after.is_empty() => {
                let (kind, content) = after.split_once(char::is_whitespace).unwrap_or((after, ""));
                call(
                    "create_artifact",
                    json!({ "name": first, "kind": kind, "content": content }),
                )
            }
            "/note" if !rest.is_empty() => call(
                "write_note",
                json!({
                    "path": format!("Inbox/{}.md", slug(rest)),
                    "content": format!("{rest}\n"),
                }),
            ),
            "/find" if !rest.is_empty() => call("search_notes", json!({ "query": rest })),
            "/context" => say(match instructions {
                Some(i) => format!(
                    "Project instructions are loaded from {} ({} bytes).",
                    i.file,
                    i.text.len()
                ),
                None => "This project has no AGENTS.md or CLAUDE.md.".into(),
            }),
            _ => say(STUB_HELP.into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Vec<Event> {
        vec![Event {
            seq: 1,
            ts_ms: 0,
            kind: EventKind::UserMessage { text: text.into() },
        }]
    }

    #[tokio::test]
    async fn commands_map_to_tools() {
        let turn = StubModel
            .next_turn(&user("/note Ship the Lantern build!"), None)
            .await
            .unwrap();
        assert_eq!(turn.tool_calls[0].tool, "write_note");
        assert_eq!(
            turn.tool_calls[0].input["path"],
            "Inbox/ship-the-lantern-build.md"
        );
        let turn = StubModel
            .next_turn(&user("/artifact chart html <p>hi there</p>"), None)
            .await
            .unwrap();
        assert_eq!(turn.tool_calls[0].input["content"], "<p>hi there</p>");
        let turn = StubModel.next_turn(&user("/context"), None).await.unwrap();
        assert!(turn.text.contains("no AGENTS.md"));
    }
}
