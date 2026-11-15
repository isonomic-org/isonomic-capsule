//! Assemble the harness host from the engine's parts.

use std::sync::Arc;

use harness_core::Profile;
use harness_host::{
    memory_config, Deps, Host, InProcessExecutor, MemoryBlobs, ModelClient, Spawner,
};
use harness_toolkit::toolkit;

use crate::deps::{EngineLocale, NoClientTools, TokioSpawner, WallClock};
use crate::{EngineAgents, EngineScopes, SqliteSessionStore};

/// What the engine supplies to the agents host.
pub struct Parts {
    pub store: Arc<SqliteSessionStore>,
    pub scopes: Arc<EngineScopes>,
    pub model: Arc<dyn ModelClient>,
    pub locale: EngineLocale,
    pub profile: capsule_core::Profile,
    /// Offer the harness's engine tools (ask the person, a to-do list, a
    /// plan, the time) to sessions whose preset allows them. Off for V1:
    /// local models take no tools, and with none offered the chat preset
    /// leaves its tools and approvals paragraph out of the prompt.
    pub engine_tools: bool,
}

fn profile(p: capsule_core::Profile) -> Profile {
    match p {
        capsule_core::Profile::Low => Profile::Low,
        capsule_core::Profile::Standard => Profile::Standard,
        capsule_core::Profile::High => Profile::High,
    }
}

/// Build the host, seal turns a previous engine left open, and serve.
/// Limits and the policy ceiling are the harness's defaults; attachments
/// are kept in memory until the host's blob store is backed by the
/// engine's (V1 Chat sends none).
pub async fn start(parts: Parts) -> Result<EngineAgents, harness_host::HostError> {
    let spawner: Arc<dyn Spawner> = Arc::new(TokioSpawner);
    let deps = Deps {
        store: parts.store.clone(),
        model: parts.model,
        clients: Arc::new(NoClientTools),
        scopes: parts.scopes,
        blobs: Arc::new(MemoryBlobs::default()),
        spawner: Arc::clone(&spawner),
        clock: Arc::new(WallClock),
        executor: Arc::new(InProcessExecutor::new(spawner)),
        locale: Arc::new(parts.locale.clone()),
    };
    let mut config = memory_config();
    config.tools = match parts.engine_tools {
        true => toolkit(Arc::new(parts.locale)),
        false => Vec::new(),
    };
    config.profile = Some(profile(parts.profile));
    let host = Host::new(deps, config);
    host.recover().await?;
    Ok(EngineAgents::new(host, parts.store))
}
