//! The Unchained core: everything that runs on the host and does not care
//! which client is watching.

pub mod model;
pub mod permissions;
pub mod session;
pub mod tools;
pub mod workspace;

pub use model::{ModelAdapter, ModelTurn, StubModel, ToolRequest};
pub use permissions::{Effect, Mode, Policy, Rule, Verdict};
pub use session::{Session, SessionManager, SubmitError};
pub use tools::{Tool, ToolRegistry};
pub use workspace::Workspace;
