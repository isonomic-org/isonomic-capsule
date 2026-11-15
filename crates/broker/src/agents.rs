//! The seam to the agents host: the broker checks the token and the
//! capability, then hands every `agents` call here. The host itself lives
//! in the `agents` crate, which depends on this one, so the broker only
//! names the trait.

use std::future::Future;
use std::pin::Pin;

use isonomic_proto::{Call, ErrorCode, Manifest, Reply, ServerMessage, WireError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Starts a subscription's forwarding task, given its id and the outbox.
pub type Starter = Box<dyn FnOnce(u64, mpsc::Sender<ServerMessage>) -> JoinHandle<()> + Send>;

/// What an agents call leads to.
pub enum AgentsAnswer {
    Reply(Reply),
    /// `sessions.subscribe`: forward events once `session_stream` is sent.
    Stream(Starter),
}

pub trait Agents: Send + Sync + 'static {
    /// Answer one agents call for `manifest`'s app (`None` is the shell).
    /// `proto::auth::authorize` has already run.
    fn answer<'a>(
        &'a self,
        manifest: Option<&'a Manifest>,
        call: Call,
    ) -> BoxFuture<'a, Result<AgentsAnswer, WireError>>;
}

/// No agents host: every call is answered `unknown_method`.
pub struct NoAgents;

impl Agents for NoAgents {
    fn answer<'a>(
        &'a self,
        _: Option<&'a Manifest>,
        call: Call,
    ) -> BoxFuture<'a, Result<AgentsAnswer, WireError>> {
        let method = call.method();
        Box::pin(async move {
            Err(WireError::new(
                ErrorCode::UnknownMethod,
                format!("this engine does not serve {method} yet"),
            ))
        })
    }
}
