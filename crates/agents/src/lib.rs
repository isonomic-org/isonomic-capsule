//! Agents: hosts harness sessions inside the engine (plan:
//! `docs/plans/agents.md`). This crate holds adapters over the harness
//! host's traits only; the loop, policy and event ordering are the harness's.
//!
//! Built so far: the session log and registry on SQLite, and the token
//! resolver with user-action tokens.

mod scopes;
mod store;

pub use scopes::{EngineScopes, USER_ACTION_TTL_SECS};
pub use store::SqliteSessionStore;
