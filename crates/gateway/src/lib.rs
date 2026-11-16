//! The model gateway (SDK request R2): the `models` interface on local
//! models, through isonomic-inference.
//!
//! The catalogue is the model library's manifest; a model is installed when
//! its file is in the Capsule's model directory at full size. Each model
//! gets its own llama.cpp worker, started on first use and unloaded when
//! idle by the inference supervisor. The engine's memory gate is the only
//! real one: it is checked here before a worker is started, so a refusal
//! carries the gate's own numbers, and handed to the supervisor as well.
//! Remote models are not served: V1 is local only.

mod catalogue;
mod chat_worker;
mod disk;
mod downloads;
pub mod fetch;
mod service;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use isonomic_inference_core::{Capability, ChatMessage, ChatRequest, ReasoningChoice};
use isonomic_llama::{LlamaConfig, LlamaSupervisor, ServerMode};
use isonomic_model_library::{Manifest, ModelEntry};
use isonomic_proto::auth::{authorize_model, Caller};
use isonomic_proto::models::{ModelState, Role, StreamRequest};
use isonomic_proto::{ErrorCode, MemoryRefusal, WireError};
use isonomic_provider::{ChatStream, GatewayError, InferenceProvider, LocalLlama};

pub use catalogue::{Catalogue, SLOW_CHAT_MODEL};
pub use chat_worker::{CHAT_SAMPLING, CHAT_SLOTS, SLOT_CONTEXT};
pub use disk::{DiskSpace, FixedDisk, SystemDisk, DISK_HEADROOM};

/// No local model runs on less memory than this: 16 GiB.
pub const MEMORY_FLOOR: u64 = 16 << 30;

/// What the gateway knows about the machine, for `models.recommended`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// The engine's profile: `low`, `standard` or `high`.
    pub profile: String,
    pub total_memory_bytes: u64,
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            profile: "standard".into(),
            total_memory_bytes: MEMORY_FLOOR,
        }
    }
}

/// Adjusts each worker's configuration (tests point it at a fake server).
pub type Configure = Box<dyn Fn(&mut LlamaConfig) + Send + Sync>;

/// Where a model's file is downloaded from: the manifest's own source by
/// default (https only, checksum pinned). Tests serve it from loopback.
pub type Source = Box<dyn Fn(&ModelEntry) -> isonomic_model_library::ModelSpec + Send + Sync>;

pub struct Gateway {
    catalogue: Catalogue,
    /// The llama.cpp server program; `None` when the bundle has none.
    server: Option<PathBuf>,
    gate: Arc<dyn capsule_core::RamGate>,
    configure: Configure,
    workers: Mutex<HashMap<String, Arc<LocalLlama>>>,
    downloads: downloads::Downloads,
    machine: Machine,
    source: Source,
    disk: Arc<dyn DiskSpace>,
    stall: fetch::Stall,
    /// How long a worker may sit idle before it is unloaded; `None` in the
    /// outer option keeps the inference supervisor's own default.
    idle_unload: Option<Option<std::time::Duration>>,
}

/// The engine's memory gate, as the inference supervisor asks for it.
struct Gate(Arc<dyn capsule_core::RamGate>);

impl isonomic_inference_core::RamGate for Gate {
    fn admit(&self, bytes: u64) -> Result<(), isonomic_inference_core::RamDenial> {
        self.0
            .admit(bytes)
            .map_err(|d| isonomic_inference_core::RamDenial {
                required_bytes: d.required_bytes,
                available_bytes: d.available_bytes,
                reason: d.reason,
            })
    }
}

pub(crate) fn unavailable(message: impl Into<String>) -> WireError {
    WireError::new(ErrorCode::ModelUnavailable, message)
}

/// What the inference layer said, as the wire says it.
pub(crate) fn failure(error: GatewayError) -> WireError {
    match error {
        GatewayError::InsufficientRam { reason } => {
            WireError::new(ErrorCode::InsufficientMemory, reason)
        }
        GatewayError::ModelMissing { .. } | GatewayError::ModelDownloading { .. } => {
            unavailable(format!("the model is not installed: {error}"))
        }
        other => unavailable(other.to_string()),
    }
}

impl Gateway {
    /// Models from `manifest`, files in `models_dir`, workers from `server`.
    /// `slow` picks the smallest installed chat model by default.
    pub fn new(
        manifest: Manifest,
        models_dir: PathBuf,
        server: Option<PathBuf>,
        gate: Arc<dyn capsule_core::RamGate>,
        slow: bool,
    ) -> Self {
        Self {
            catalogue: Catalogue::new(manifest, models_dir, slow),
            server,
            gate,
            configure: Box::new(|_| {}),
            workers: Mutex::new(HashMap::new()),
            downloads: downloads::Downloads::default(),
            machine: Machine::default(),
            source: Box::new(ModelEntry::spec),
            disk: Arc::new(SystemDisk),
            stall: fetch::stall_limit(),
            idle_unload: None,
        }
    }

    pub fn with_source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    /// Where the free disk space before a download is read.
    pub fn with_disk(mut self, disk: Arc<dyn DiskSpace>) -> Self {
        self.disk = disk;
        self
    }

    /// How long a model may sit idle before its worker is unloaded: the
    /// profile's `model_idle_unload()` (`None`: never).
    pub fn with_idle_unload(mut self, idle: Option<std::time::Duration>) -> Self {
        self.idle_unload = Some(idle);
        self
    }

    /// When a silent download counts as stalled (tests end it on demand).
    pub fn with_stall(mut self, stall: fetch::Stall) -> Self {
        self.stall = stall;
        self
    }

    pub fn with_machine(mut self, machine: Machine) -> Self {
        self.machine = machine;
        self
    }

    pub fn with_configure(mut self, configure: Configure) -> Self {
        self.configure = configure;
        self
    }

    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    /// The model a call runs on: the one it names, or the default for
    /// `capability`. It must be installed and able to do it, and the
    /// caller must be allowed to use it.
    pub fn choose(
        &self,
        caller: Caller<'_>,
        named: Option<&str>,
        capability: Capability,
    ) -> Result<&ModelEntry, WireError> {
        let entry = match named {
            Some(id) => self
                .catalogue
                .get(id)
                .ok_or_else(|| WireError::new(ErrorCode::UnknownModel, format!("no model {id}")))?,
            None => self.catalogue.default_for(capability).ok_or_else(|| {
                let kind = capability.as_str();
                WireError::new(
                    ErrorCode::UnknownModel,
                    format!("no {kind} model is installed"),
                )
            })?,
        };
        let id = entry.meta.id.as_str();
        authorize_model(caller, id, false)?;
        if !self.catalogue.installed(entry) {
            return Err(unavailable(format!("{id} is not installed")));
        }
        if !entry.meta.supports(capability) {
            let kind = capability.as_str();
            let message = format!("{id} cannot do {kind}");
            return Err(WireError::new(ErrorCode::Unsupported, message));
        }
        Ok(entry)
    }

    /// The memory gate's answer for loading `entry`, with its numbers.
    fn admit(&self, entry: &ModelEntry) -> Result<(), WireError> {
        if self.state(entry) == ModelState::Ready {
            return Ok(());
        }
        self.gate.admit(entry.meta.size_bytes).map_err(|d| {
            WireError::memory(MemoryRefusal {
                required_bytes: d.required_bytes,
                available_bytes: d.available_bytes,
                reason: d.reason,
            })
        })
    }

    /// The worker for `entry`, made on first use; it starts on first call.
    fn worker(&self, entry: &ModelEntry, mode: ServerMode) -> Result<Arc<LocalLlama>, WireError> {
        let id = entry.meta.id.as_str().to_string();
        let mut workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(worker) = workers.get(&id) {
            return Ok(Arc::clone(worker));
        }
        let Some(config) = self.launch_config(entry, mode) else {
            return Err(unavailable("the local model runtime is not installed"));
        };
        let gate = Arc::new(Gate(Arc::clone(&self.gate)));
        let supervisor = Arc::new(LlamaSupervisor::new(config, gate));
        let worker = Arc::new(LocalLlama::for_model(supervisor, &entry.meta));
        workers.insert(id, Arc::clone(&worker));
        Ok(worker)
    }

    /// What `entry`'s worker is launched with in `mode`; `None` without the
    /// bundled server.
    pub fn launch_config(&self, entry: &ModelEntry, mode: ServerMode) -> Option<LlamaConfig> {
        let server = self.server.as_ref()?;
        let mut config = LlamaConfig::new(server.clone(), self.catalogue.path(entry));
        config.mode = mode;
        if let Some(idle) = self.idle_unload {
            config.idle_unload = idle;
        }
        if mode == ServerMode::Chat {
            chat_worker::configure(&mut config, entry, self.catalogue.dir());
        }
        (self.configure)(&mut config);
        Some(config)
    }

    /// How a model is doing: downloading, not installed, or its worker's
    /// state.
    pub fn state(&self, entry: &ModelEntry) -> ModelState {
        if self.downloads.received(entry.meta.id.as_str()).is_some() {
            return ModelState::Downloading;
        }
        if !self.catalogue.installed(entry) {
            return ModelState::NotInstalled;
        }
        let workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
        let Some(worker) = workers.get(entry.meta.id.as_str()) else {
            return ModelState::Stopped;
        };
        match worker.supervisor().state() {
            isonomic_llama::ServiceState::Stopped => ModelState::Stopped,
            isonomic_llama::ServiceState::Starting => ModelState::Starting,
            isonomic_llama::ServiceState::Healthy => ModelState::Ready,
            isonomic_llama::ServiceState::Degraded => ModelState::Degraded,
        }
    }

    /// Open a chat stream: choose, check memory, start the worker if need
    /// be, send the request. The model's id comes back with the stream.
    pub async fn chat(
        &self,
        caller: Caller<'_>,
        request: StreamRequest,
    ) -> Result<(String, ChatStream), WireError> {
        if !request.tools.is_empty() {
            let message = "local models do not take tools yet";
            return Err(WireError::new(ErrorCode::Unsupported, message));
        }
        let entry = self.choose(caller, request.model.as_deref(), Capability::Chat)?;
        self.admit(entry)?;
        let worker = self.worker(entry, ServerMode::Chat)?;
        let id = entry.meta.id.as_str().to_string();
        let chat = ChatRequest {
            model: Some(id.clone()),
            messages: request.messages.into_iter().map(message).collect(),
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            reasoning: request.reasoning.map(|on| match on {
                true => ReasoningChoice::On,
                false => ReasoningChoice::Off,
            }),
        };
        let stream = worker.chat(chat).await.map_err(failure)?;
        Ok((id, stream))
    }

    /// Embed every input, in order.
    pub async fn embed(
        &self,
        caller: Caller<'_>,
        named: Option<&str>,
        input: Vec<String>,
    ) -> Result<(String, Vec<Vec<f32>>), WireError> {
        let entry = self.choose(caller, named, Capability::Embeddings)?;
        self.admit(entry)?;
        let worker = self.worker(entry, ServerMode::Embedding)?;
        let id = entry.meta.id.as_str().to_string();
        let request = isonomic_inference_core::EmbedRequest {
            model: Some(id.clone()),
            input,
        };
        let embeddings = worker.embeddings(request).await.map_err(failure)?;
        Ok((id, embeddings.vectors))
    }
}

fn message(m: isonomic_proto::models::Message) -> ChatMessage {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    ChatMessage {
        role: role.to_string(),
        content: m.content,
    }
}
