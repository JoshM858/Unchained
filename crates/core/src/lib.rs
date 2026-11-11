//! The Unchained core: everything that runs on the host and does not care
//! which client is watching.

pub mod artifacts;
pub mod model;
pub mod notes;
pub mod permissions;
pub mod project;
pub mod scratch;
pub mod session;
pub mod tools;
pub mod workspace;

pub use artifacts::ArtifactStore;
pub use model::{ModelAdapter, ModelTurn, StubModel, ToolRequest};
pub use permissions::{Effect, Mode, Policy, Rule, Verdict};
pub use project::{Instructions, ProjectStore};
pub use session::{Session, SessionManager, SubmitError};
pub use tools::{Tool, ToolEnv, ToolRegistry};
pub use workspace::Workspace;
