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

pub use catalogue::Catalogue;

/// Adjusts each worker's configuration (tests point it at a fake server).
pub type Configure = Box<dyn Fn(&mut LlamaConfig) + Send + Sync>;

pub struct Gateway {
    catalogue: Catalogue,
    /// The llama.cpp server program; `None` when the bundle has none.
    server: Option<PathBuf>,
    gate: Arc<dyn capsule_core::RamGate>,
    configure: Configure,
    workers: Mutex<HashMap<String, Arc<LocalLlama>>>,
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
        }
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
        let Some(server) = &self.server else {
            return Err(unavailable("the local model runtime is not installed"));
        };
        let mut config = LlamaConfig::new(server.clone(), self.catalogue.path(entry));
        config.mode = mode;
        (self.configure)(&mut config);
        let gate = Arc::new(Gate(Arc::clone(&self.gate)));
        let supervisor = Arc::new(LlamaSupervisor::new(config, gate));
        let worker = Arc::new(LocalLlama::for_model(supervisor, &entry.meta));
        workers.insert(id, Arc::clone(&worker));
        Ok(worker)
    }

    /// How a model's worker is doing.
    pub fn state(&self, entry: &ModelEntry) -> ModelState {
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
