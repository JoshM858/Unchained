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
    pub project: String,
    pub workspace: String,
    pub created_ms: u64,
    pub running: bool,
    pub last_seq: u64,
}

/// How far the agent may touch a project's notes vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VaultMode {
    /// The agent may search and read notes. Writes are refused.
    #[default]
    ReadOnly,
    /// Every note write asks for approval first.
    ReadWriteAsk,
    /// The agent writes notes without asking.
    ReadWrite,
}

impl VaultMode {
    pub fn as_str(self) -> &'static str {
        match self {
            VaultMode::ReadOnly => "read-only",
            VaultMode::ReadWriteAsk => "read-write-ask",
            VaultMode::ReadWrite => "read-write",
        }
    }
}

impl std::str::FromStr for VaultMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read-only" => Ok(VaultMode::ReadOnly),
            "read-write-ask" => Ok(VaultMode::ReadWriteAsk),
            "read-write" => Ok(VaultMode::ReadWrite),
            other => Err(format!(
                "unknown vault mode '{other}' (expected read-only, read-write-ask or read-write)"
            )),
        }
    }
}

/// A project: a workspace the agent works in, its instructions, sessions,
/// artifacts, and optionally an Obsidian-style notes vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// The name of a registered workspace.
    pub workspace: String,
    /// Absolute path of the notes vault folder, if any.
    pub vault: Option<String>,
    pub vault_mode: VaultMode,
    pub created_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProject {
    pub name: String,
    pub workspace: String,
    #[serde(default)]
    pub vault: Option<String>,
    #[serde(default)]
    pub vault_mode: Option<VaultMode>,
}

/// Partial update. `vault: null` detaches the vault; an absent field is unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateProject {
    #[serde(default, deserialize_with = "double_option")]
    pub vault: Option<Option<String>>,
    #[serde(default)]
    pub vault_mode: Option<VaultMode>,
}

fn double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub version: u32,
    pub bytes: u64,
    pub ts_ms: u64,
}

/// An artifact is a named, versioned, self-contained result the agent made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactMeta {
    pub name: String,
    /// `html`, `svg`, `markdown` or `code`.
    pub kind: String,
    pub versions: Vec<ArtifactVersion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteEntry {
    /// Vault-relative path, with `/` separators.
    pub path: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveInfo {
    /// File name of the `.tar.gz`; pass it to restore.
    pub name: String,
    pub session: String,
    pub archived_ms: u64,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub root: String,
}

/// One entry in a workspace directory listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSession {
    pub project: String,
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
