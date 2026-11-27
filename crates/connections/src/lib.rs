// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tool servers the person connects, kept and spoken to by the engine.
//!
//! [`Connections`] keeps the list in `connections.json` (no secrets) and
//! each connection's secrets in a [`Vault`] (the Mac login keychain). It
//! starts a local server as a command with an allow-listed environment, or
//! reaches a remote one over HTTP, signs in to a remote one through the
//! person's browser and a loopback page, and calls tools. Every change of
//! state is persisted first, then published. [`wire::Wire`] answers the
//! broker's `connections.*` calls; [`service::Service`] gives the harness
//! the connected apps.

pub mod http;
mod loopback;
pub mod oauth;
mod record;
pub mod rpc;
pub mod service;
pub mod stdio;
mod store;
mod vault;
pub mod wire;

pub use record::{Connection, NewConnection, Server, State, ToolInfo};
pub use rpc::CallOutcome;
pub use vault::{MacVault, MemoryVault, Vault};

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use record::Record;
use rpc::{BoxFuture, Failure, Session};
use store::Store;
use tokio::sync::broadcast;

/// How the engine starts and reaches servers.
#[derive(Clone, Debug)]
pub struct Options {
    /// The whole environment a local server starts with.
    pub child_env: Vec<(String, String)>,
    /// The HTTP client for remote servers and sign-ins.
    pub client: reqwest::Client,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            child_env: Vec::new(),
            client: http::client(),
        }
    }
}

struct Live {
    record: Record,
    state: State,
    error: Option<String>,
    tools: Vec<ToolInfo>,
    session: Option<Arc<Session>>,
    /// Bumped on each (re)connect, so a stale attempt never overwrites a
    /// newer one.
    attempt: u64,
    /// The metadata address the server named when it asked for a sign-in.
    hint: Option<String>,
}

impl Live {
    fn view(&self) -> Connection {
        Connection {
            id: self.record.id.clone(),
            name: self.record.name.clone(),
            key: self.record.key.clone(),
            server: self.record.server.clone(),
            state: self.state,
            error: self.error.clone(),
            tools: self.tools.len() as u32,
        }
    }
}

/// The person's connections.
pub struct Connections {
    store: Store,
    vault: Arc<dyn Vault>,
    options: Options,
    live: Mutex<Vec<Live>>,
    events: broadcast::Sender<Connection>,
    next: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A remote connection's sign-in, read from the vault and refreshed there.
struct Granted {
    vault: Arc<dyn Vault>,
    client: reqwest::Client,
    name: String,
    grant: tokio::sync::Mutex<oauth::Grant>,
    token: Mutex<String>,
}

impl http::Bearer for Granted {
    fn token(&self) -> Option<String> {
        Some(lock(&self.token).clone())
    }

    fn refresh(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let mut grant = self.grant.lock().await;
            if oauth::refresh(&self.client, &mut grant).await.is_err() {
                return false;
            }
            let Ok(bytes) = serde_json::to_vec(&*grant) else {
                return false;
            };
            if self.vault.set(&self.name, &bytes).is_err() {
                return false;
            }
            *lock(&self.token) = grant.access_token.clone();
            true
        })
    }
}

impl Connections {
    /// Load the list from `dir` and start connecting each one.
    pub fn open(dir: &Path, vault: Arc<dyn Vault>, options: Options) -> Result<Arc<Self>, String> {
        let store = Store::new(dir);
        let live = store
            .load()?
            .into_iter()
            .map(|record| Live {
                record,
                state: State::Connecting,
                error: None,
                tools: Vec::new(),
                session: None,
                attempt: 0,
                hint: None,
            })
            .collect();
        let (events, _) = broadcast::channel(64);
        let this = Arc::new(Self {
            store,
            vault,
            options,
            live: Mutex::new(live),
            events,
            next: AtomicU64::new(1),
        });
        let ids: Vec<String> = lock(&this.live)
            .iter()
            .map(|l| l.record.id.clone())
            .collect();
        for id in ids {
            Arc::clone(&this).connect(id);
        }
        Ok(this)
    }

    /// Each change to a connection, as it happens.
    pub fn subscribe(&self) -> broadcast::Receiver<Connection> {
        self.events.subscribe()
    }

    pub fn list(&self) -> Vec<Connection> {
        lock(&self.live).iter().map(Live::view).collect()
    }

    pub fn get(&self, id: &str) -> Option<Connection> {
        lock(&self.live)
            .iter()
            .find(|l| l.record.id == id)
            .map(Live::view)
    }

    /// Keep a new connection, then start connecting it.
    pub fn add(self: &Arc<Self>, new: NewConnection) -> Result<Connection, String> {
        if new.name.trim().is_empty() {
            return Err("A connection needs a name.".into());
        }
        match &new.server {
            Server::Remote { url } => {
                if !(url.starts_with("https://") || is_loopback_url(url)) {
                    return Err("A tool server's address must start with https://.".into());
                }
            }
            Server::Local { command, .. } if command.trim().is_empty() => {
                return Err("A local tool server needs a command.".into())
            }
            Server::Local { .. } => {}
        }
        let id = self.mint();
        if let Some(token) = &new.token {
            self.vault.set(&vault::token_name(&id), token.as_bytes())?;
        }
        let record = Record {
            id: id.clone(),
            name: new.name,
            key: new.key,
            server: new.server,
        };
        let view = {
            let mut live = lock(&self.live);
            let mut records: Vec<Record> = live.iter().map(|l| l.record.clone()).collect();
            records.push(record.clone());
            // Persist, then publish.
            self.store.save(&records)?;
            live.push(Live {
                record,
                state: State::Connecting,
                error: None,
                tools: Vec::new(),
                session: None,
                attempt: 0,
                hint: None,
            });
            live.last().map(Live::view).expect("just pushed")
        };
        let _ = self.events.send(view.clone());
        Arc::clone(self).connect(id);
        Ok(view)
    }

    /// Forget a connection, its secrets and its session.
    pub async fn remove(&self, id: &str) -> Result<(), String> {
        let session = {
            let mut live = lock(&self.live);
            let Some(at) = live.iter().position(|l| l.record.id == id) else {
                return Err(format!("There is no connection {id}."));
            };
            let records: Vec<Record> = live
                .iter()
                .filter(|l| l.record.id != id)
                .map(|l| l.record.clone())
                .collect();
            self.store.save(&records)?;
            live.remove(at).session
        };
        let _ = self.vault.delete(&vault::token_name(id));
        let _ = self.vault.delete(&vault::sign_in_name(id));
        if let Some(session) = session {
            session.close().await;
        }
        Ok(())
    }

    /// The tools a connection offers (none until it is ready).
    pub fn tools(&self, id: &str) -> Result<Vec<ToolInfo>, String> {
        lock(&self.live)
            .iter()
            .find(|l| l.record.id == id)
            .map(|l| l.tools.clone())
            .ok_or_else(|| format!("There is no connection {id}."))
    }

    /// Every ready connection with its tools.
    pub fn ready(&self) -> Vec<(Connection, Vec<ToolInfo>)> {
        lock(&self.live)
            .iter()
            .filter(|l| l.state == State::Ready)
            .map(|l| (l.view(), l.tools.clone()))
            .collect()
    }

    /// Call `tool` on a ready connection with `arguments`, a JSON object.
    pub async fn call(
        &self,
        id: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<CallOutcome, String> {
        let session = {
            let live = lock(&self.live);
            let l = live
                .iter()
                .find(|l| l.record.id == id)
                .ok_or_else(|| format!("There is no connection {id}."))?;
            if !l.tools.iter().any(|t| t.name == tool) {
                return Err(format!("{} has no tool {tool}.", l.record.name));
            }
            l.session
                .clone()
                .ok_or_else(|| format!("{} is not connected.", l.record.name))?
        };
        match session.call(tool, arguments).await {
            Ok(out) => Ok(out),
            Err(Failure::SignIn(hint)) => {
                self.set_state(id, State::NeedsSignIn, None, hint);
                Err("The connection needs a sign-in again.".into())
            }
            Err(Failure::Failed(message)) => Err(message),
        }
    }

    /// Start a remote server's sign-in: the page to open in the person's
    /// browser. The outcome arrives as a change to the connection.
    pub async fn sign_in(self: &Arc<Self>, id: &str) -> Result<String, String> {
        let (url, hint) = {
            let live = lock(&self.live);
            let l = live
                .iter()
                .find(|l| l.record.id == id)
                .ok_or_else(|| format!("There is no connection {id}."))?;
            match &l.record.server {
                Server::Remote { url } => (url.clone(), l.hint.clone()),
                Server::Local { .. } => {
                    return Err("A local tool server has no browser sign-in.".into())
                }
            }
        };
        let client = &self.options.client;
        let endpoints = oauth::discover(client, &url, hint.as_deref()).await?;
        let page = loopback::Loopback::bind().await?;
        let redirect = page.redirect();
        let (client_id, secret) = oauth::register(client, &endpoints, &redirect).await?;
        let pending = oauth::Pending::new(endpoints, client_id, secret, redirect)?;
        let browser = pending.url()?;
        let this = Arc::clone(self);
        let id = id.to_string();
        tokio::spawn(async move {
            let done = match page.code(&pending.state).await {
                Ok(code) => pending.finish(&this.options.client, &code).await,
                Err(e) => Err(e),
            };
            let kept = done.and_then(|grant| {
                let bytes = serde_json::to_vec(&grant).map_err(|e| e.to_string())?;
                this.vault.set(&vault::sign_in_name(&id), &bytes)
            });
            match kept {
                Ok(()) => this.connect(id),
                Err(e) => this.set_state(&id, State::Failed, Some(e), None),
            }
        });
        Ok(browser)
    }

    fn mint(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("conn-{now:x}-{n}")
    }

    fn set_state(&self, id: &str, state: State, error: Option<String>, hint: Option<String>) {
        let view = {
            let mut live = lock(&self.live);
            let Some(l) = live.iter_mut().find(|l| l.record.id == id) else {
                return;
            };
            l.state = state;
            l.error = error;
            if hint.is_some() {
                l.hint = hint;
            }
            if state != State::Ready {
                l.session = None;
                l.tools.clear();
            }
            l.view()
        };
        let _ = self.events.send(view);
    }

    /// How a remote connection authenticates: a pasted token, a sign-in,
    /// or nothing.
    fn bearer(&self, id: &str) -> Result<Option<Arc<dyn http::Bearer>>, String> {
        if let Some(token) = self.vault.get(&vault::token_name(id))? {
            let token = String::from_utf8(token).map_err(|_| "a token that is not text")?;
            return Ok(Some(Arc::new(http::Pasted(token))));
        }
        let name = vault::sign_in_name(id);
        let Some(bytes) = self.vault.get(&name)? else {
            return Ok(None);
        };
        let grant: oauth::Grant = serde_json::from_slice(&bytes)
            .map_err(|_| "The saved sign-in could not be read; sign in again.".to_string())?;
        Ok(Some(Arc::new(Granted {
            vault: Arc::clone(&self.vault),
            client: self.options.client.clone(),
            name,
            token: Mutex::new(grant.access_token.clone()),
            grant: tokio::sync::Mutex::new(grant),
        })))
    }

    async fn open_session(&self, id: &str, server: &Server) -> Result<Session, Failure> {
        let transport: Box<dyn rpc::Transport> = match server {
            Server::Local { command, args } => Box::new(stdio::StdioServer::start(
                command,
                args,
                &self.options.child_env,
            )?),
            Server::Remote { url } => {
                let bearer = self.bearer(id).map_err(Failure::Failed)?;
                let client = self.options.client.clone();
                Box::new(http::Http::new(client, url.clone(), bearer))
            }
        };
        Session::open(transport).await
    }

    /// (Re)connect in the background.
    fn connect(self: Arc<Self>, id: String) {
        let (server, attempt, view) = {
            let mut live = lock(&self.live);
            let Some(l) = live.iter_mut().find(|l| l.record.id == id) else {
                return;
            };
            l.attempt += 1;
            l.state = State::Connecting;
            l.error = None;
            (l.record.server.clone(), l.attempt, l.view())
        };
        let _ = self.events.send(view);
        tokio::spawn(async move {
            let result = match self.open_session(&id, &server).await {
                Ok(session) => match session.tools().await {
                    Ok(tools) => Ok((session, tools)),
                    Err(e) => {
                        session.close().await;
                        Err(e)
                    }
                },
                Err(e) => Err(e),
            };
            self.settle(&id, attempt, result);
        });
    }

    fn settle(&self, id: &str, attempt: u64, result: Result<(Session, Vec<ToolInfo>), Failure>) {
        let view = {
            let mut live = lock(&self.live);
            let Some(l) = live.iter_mut().find(|l| l.record.id == id) else {
                return;
            };
            if l.attempt != attempt {
                return;
            }
            match result {
                Ok((session, tools)) => {
                    l.state = State::Ready;
                    l.error = None;
                    l.tools = tools;
                    l.session = Some(Arc::new(session));
                }
                Err(Failure::SignIn(hint)) => {
                    l.state = State::NeedsSignIn;
                    l.error = None;
                    l.hint = hint;
                }
                Err(Failure::Failed(e)) => {
                    l.state = State::Failed;
                    l.error = Some(e);
                }
            }
            l.view()
        };
        let _ = self.events.send(view);
    }
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.to_ascii_lowercase())
}

/// Plain HTTP only to this computer (the tests' servers, a local proxy).
fn is_loopback_url(url: &str) -> bool {
    url.starts_with("http://")
        && matches!(
            host_of(url).as_deref(),
            Some("127.0.0.1") | Some("localhost")
        )
}
