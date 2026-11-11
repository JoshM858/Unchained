//! Wire types shared by the daemon and every client (web UI, TUI, CLI).
//!
//! A session is an append-only log of [`Event`]s. Clients attach over a
//! WebSocket, receive every event after the sequence number they already
//! hold, and send [`ClientMessage`]s back. The log is the source of truth:
//! replay, resume and multi-device sync all fall out of it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One entry in a session's event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// 1-based, gapless position in the session log.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub ts_ms: u64,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    UserMessage {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    /// The model asked for a tool to run.
    ToolCall {
        call_id: String,
        tool: String,
        input: Value,
    },
    /// The permission engine answered "ask"; the turn is paused until a
    /// client replies with [`ClientMessage::PermissionDecision`].
    PermissionRequested {
        call_id: String,
        tool: String,
        input: Value,
    },
    PermissionResolved {
        call_id: String,
        allowed: bool,
        /// `"client"`, `"timeout"` or `"interrupt"`.
        by: String,
    },
    ToolResult {
        call_id: String,
        ok: bool,
        output: String,
    },
    Error {
        message: String,
    },
    /// The agent has stopped and the session accepts a new user message.
    TurnFinished,
}

/// Messages a client sends over the session WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    UserMessage { text: String },
    PermissionDecision { call_id: String, allow: bool },
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub workspace: String,
    pub created_ms: u64,
    pub running: bool,
    pub last_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSession {
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Login {
    pub token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_with_flat_tag() {
        let e = Event {
            seq: 3,
            ts_ms: 10,
            kind: EventKind::ToolResult {
                call_id: "c1".into(),
                ok: true,
                output: "hi".into(),
            },
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["seq"], 3);
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), e);
    }

    #[test]
    fn client_message_parses() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"permission_decision","call_id":"c","allow":false}"#)
                .unwrap();
        assert_eq!(
            m,
            ClientMessage::PermissionDecision {
                call_id: "c".into(),
                allow: false
            }
        );
    }
}
