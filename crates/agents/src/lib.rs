//! Agents: hosts harness sessions inside the engine (plan:
//! `docs/plans/agents.md`). This crate holds adapters over the harness
//! host's traits only; the loop, policy and event ordering are the harness's.
//!
//! Built so far: the session log and registry on SQLite, and the token
//! resolver with user-action tokens.

mod convert;
pub mod deps;
mod model;
mod scopes;
mod service;
mod start;
mod store;

pub use model::GatewayModel;
pub use scopes::{EngineScopes, USER_ACTION_TTL_SECS};
pub use service::{EngineAgents, DELTA_TICK};
pub use start::{start, Parts};
pub use store::SqliteSessionStore;
