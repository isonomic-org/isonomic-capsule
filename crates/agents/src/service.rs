//! The `agents` interface served by the harness host: each method maps one
//! to one onto a host call. The broker has checked the token and the
//! capability; the host decides what each caller reaches.
//!
//! Approval, question and call ids are minted per session, so the wire and
//! the host both name the target by (session, id). The host answers a late
//! client tool result with `Ok` and an audit record, so a call id that was
//! never issued in the session is checked here, to stay `not_found` on the
//! wire. A stream awaits durable events and live deltas alike
//! (`Subscription::next_update`).

use std::sync::Arc;

use broker::agents::{AgentsAnswer, BoxFuture, Starter};
use harness_core::{CallId, Event as LogEvent, SessionId};
use harness_host::{
    AnswerInput, AppKind, Caller, ClientOutcome, CreateOptions, Host, HostError, Input,
    Subscription, Update, UserActionToken,
};
use harness_log::SessionStore;
use isonomic_proto::agents::{EndReason, SessionEnded};
use isonomic_proto::{Call, ErrorCode, Event, Manifest, Reply, ServerMessage, WireError};
use tokio::sync::mpsc;

use gateway::Gateway;

use crate::{convert, EngineScopes};

pub struct EngineAgents {
    host: Host,
    store: Arc<dyn SessionStore>,
    /// Checks the model a session is created with; `None` passes any name
    /// (a host on a scripted model, in tests).
    gateway: Option<Arc<Gateway>>,
    /// Mints the tokens `approvals.user_action` hands the window; `None`
    /// answers that call `unsupported`.
    scopes: Option<Arc<EngineScopes>>,
}

/// The longest model name `sessions.create` takes, as the SDK checks it.
pub use isonomic_proto::agents::MAX_MODEL_NAME;

fn caller(manifest: Option<&Manifest>) -> Caller {
    match manifest {
        None => Caller::shell(),
        Some(m) => Caller::app(&m.id, AppKind::FirstParty),
    }
}

fn done(result: Result<(), HostError>) -> Result<Reply, WireError> {
    result.map(|()| Reply::Done {}).map_err(convert::error)
}

impl EngineAgents {
    /// Serve `host`, whose log is `store`. Call [`Host::recover`] before
    /// the broker serves.
    pub fn new(host: Host, store: Arc<dyn SessionStore>, gateway: Option<Arc<Gateway>>) -> Self {
        Self {
            host,
            store,
            gateway,
            scopes: None,
        }
    }

    /// Mint `approvals.user_action` tokens with `scopes`, the resolver the
    /// host verifies them with.
    pub fn with_scopes(mut self, scopes: Arc<EngineScopes>) -> Self {
        self.scopes = Some(scopes);
        self
    }

    /// The model a session is created with: checked as the SDK checks a
    /// name, then with the rule `models.stream` applies, the manifest
    /// first.
    fn session_model(
        &self,
        manifest: Option<&Manifest>,
        named: Option<String>,
    ) -> Result<Option<String>, WireError> {
        let Some(named) = named else {
            return Ok(None);
        };
        let name = isonomic_proto::agents::model_name(&named)?;
        self.check_model(manifest, name).map(Some)
    }

    /// The manifest first, so an app learns nothing about models it may
    /// not use; then the gateway's rule, when there is a gateway.
    fn check_model(&self, manifest: Option<&Manifest>, id: &str) -> Result<String, WireError> {
        if let Some(m) = manifest {
            let app = isonomic_proto::auth::Caller::App {
                id: &m.id,
                capabilities: &m.capabilities,
            };
            isonomic_proto::auth::authorize_model(app, id, false)?;
        }
        match &self.gateway {
            Some(gateway) => gateway.check_session_model(manifest, id),
            None => Ok(id.to_string()),
        }
    }

    /// An app's own stored preset names its model too, and the same rule
    /// holds: an app cannot reach a model through a preset it stored.
    /// Built-in presets are the engine's choice and are not checked, nor
    /// are the engine's own choices `local:default` and `local:smallest`.
    async fn check_preset_model(
        &self,
        manifest: Option<&Manifest>,
        who: &Caller,
        preset: &harness_core::PresetId,
    ) -> Result<(), WireError> {
        if manifest.is_none() {
            return Ok(());
        }
        let Ok(stored) = self.host.get_preset(who, preset).await else {
            // The host answers an unknown preset itself.
            return Ok(());
        };
        let (Some(_), Some(parsed)) = (&stored.owner, &stored.preset) else {
            return Ok(());
        };
        let id = parsed.model.id.as_str();
        let id = id.strip_prefix("local:").unwrap_or(id);
        if matches!(id, "default" | "smallest") {
            return Ok(());
        }
        self.check_model(manifest, id).map(|_| ())
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Whether `call` is a tool call of `session`, per its log.
    async fn has_call(&self, session: &SessionId, call: &CallId) -> bool {
        let mut after = 0;
        loop {
            let Ok(page) = self.store.read(session, after, 512).await else {
                return false;
            };
            let Some(last) = page.last() else {
                return false;
            };
            after = last.seq;
            let found = page
                .iter()
                .any(|e| matches!(&e.event, LogEvent::ToolCall(c) if &c.call == call));
            if found {
                return true;
            }
        }
    }

    async fn reply(
        &self,
        manifest: Option<&Manifest>,
        call: Call,
    ) -> Result<AgentsAnswer, WireError> {
        let who = &caller(manifest);
        let host = &self.host;
        let err = convert::error;
        let reply = match call {
            Call::SessionsCreate(c) => {
                if c.model.is_none() {
                    self.check_preset_model(manifest, who, &c.preset).await?;
                }
                let opts = CreateOptions {
                    title: c.title,
                    location_opt_in: c.location_opt_in,
                    limits: None,
                    model: self.session_model(manifest, c.model)?,
                };
                let session = host
                    .create_session(who, &c.preset, opts)
                    .await
                    .map_err(err)?;
                Reply::SessionCreated { session }
            }
            Call::SessionsList(l) => {
                let page = host
                    .list_sessions(who, convert::filter(l))
                    .await
                    .map_err(err)?;
                Reply::Sessions {
                    items: page.items.into_iter().map(convert::summary).collect(),
                    next: page.next,
                }
            }
            Call::SessionsOpen(r) => {
                let snapshot = host.open_session(who, &r.session).await.map_err(err)?;
                Reply::SessionSnapshot(Box::new(convert::snapshot(snapshot)))
            }
            Call::SessionsFork(f) => {
                let reason = f.reason.unwrap_or(harness_core::ForkReason::Branch);
                let at = convert::fork_point(f.at);
                let session = host.fork_session(who, &f.session, at, reason).await;
                Reply::SessionCreated {
                    session: session.map_err(err)?,
                }
            }
            Call::SessionsArchive(a) => {
                done(host.archive_session(who, &a.session, a.archived).await)?
            }
            Call::SessionsRename(r) => done(host.rename_session(who, &r.session, r.title).await)?,
            Call::SessionsSend(s) => {
                let input = Input {
                    content: s.content,
                    client_ref: s.client_ref,
                };
                let accepted = host.send(who, &s.session, input).await.map_err(err)?;
                Reply::Accepted {
                    queued: accepted.queued,
                }
            }
            Call::SessionsContinue(r) => done(host.continue_reply(who, &r.session).await)?,
            Call::SessionsCancel(r) => done(host.cancel(who, &r.session).await)?,
            Call::SessionsPurge(p) => {
                // `proto::auth` lets only the shell name another app, and
                // refuses the shell's unnamed purge.
                let app = match (p.app, who.scope.app()) {
                    (Some(app), _) => app,
                    (None, Some(own)) => own.clone(),
                    (None, None) => {
                        let message = "the shell must name the app to purge";
                        return Err(WireError::new(ErrorCode::BadRequest, message));
                    }
                };
                Reply::Purged(convert::purge(host.purge(who, &app).await.map_err(err)?))
            }
            Call::SessionsSubscribe(s) => {
                let sub = host.subscribe(who, &s.session, s.from_seq).await;
                return Ok(AgentsAnswer::Stream(stream(sub.map_err(err)?, s.session)));
            }
            Call::ApprovalsList(a) => {
                let all = host.list_approvals(who).await.map_err(err)?;
                let wanted = all
                    .into_iter()
                    .filter(|p| a.app.as_ref().is_none_or(|app| &p.owner == app));
                Reply::Approvals(wanted.map(convert::approval).collect())
            }
            Call::ApprovalsDecide(d) => {
                let token = d.user_action.map(UserActionToken);
                let decision = convert::decision(d.decision);
                let decided = host.decide_approval(who, &d.session, &d.approval, decision, token);
                done(decided.await)?
            }
            // `proto::auth` lets only the shell mint one.
            Call::ApprovalsUserAction(m) => {
                let Some(scopes) = &self.scopes else {
                    let message = "user actions are not minted here";
                    return Err(WireError::new(ErrorCode::Unsupported, message));
                };
                let decider = match &m.app {
                    None => Caller::shell(),
                    Some(app) => Caller::app(app.as_str(), AppKind::FirstParty),
                };
                let token = scopes
                    .mint(&decider, &m.session, &m.approval)
                    .map_err(|e| WireError::new(ErrorCode::Internal, e.to_string()))?;
                Reply::UserAction { token: token.0 }
            }
            Call::QuestionsAnswer(q) => {
                let answer = AnswerInput {
                    selected: q.selected,
                    text: q.text,
                    skipped: q.skipped,
                };
                done(
                    host.answer_question(who, &q.session, &q.question, answer)
                        .await,
                )?
            }
            Call::ToolsRegisterClient(t) => {
                done(host.register_client_tools(who, &t.session, t.specs).await)?
            }
            Call::ToolsResult(r) => {
                // The caller must reach the session, and the call must be its.
                host.open_session(who, &r.session).await.map_err(err)?;
                if !self.has_call(&r.session, &r.call).await {
                    return Err(convert::error(HostError::NotFound));
                }
                let outcome = match r.error {
                    Some(error) => ClientOutcome::Error(error, r.content),
                    None => ClientOutcome::Ok(r.content),
                };
                done(
                    host.client_tool_result(who, &r.session, &r.call, outcome)
                        .await,
                )?
            }
            Call::PresetsList(_) => {
                let presets = host.list_presets(who).await.map_err(err)?;
                Reply::Presets(presets.into_iter().map(convert::preset_summary).collect())
            }
            Call::PresetsGet(g) => {
                let preset = host.get_preset(who, &g.preset).await.map_err(err)?;
                Reply::Preset(Box::new(convert::preset_detail(preset)))
            }
            Call::PresetsPut(p) => Reply::PresetSaved {
                reference: host.put_preset(who, p.text).await.map_err(err)?,
            },
            Call::ArtifactsGet(g) => {
                let version = host.get_artifact(who, &g.artifact, g.version).await;
                let chunk = convert::artifact(version.map_err(err)?, g.offset, g.length)?;
                Reply::Artifact(Box::new(chunk))
            }
            Call::ArtifactsList(l) => {
                Reply::Artifacts(host.list_artifacts(who, &l.session).await.map_err(err)?)
            }
            other => {
                return Err(WireError::new(
                    ErrorCode::UnknownMethod,
                    format!("{} is not an agents method", other.method()),
                ))
            }
        };
        Ok(AgentsAnswer::Reply(reply))
    }
}

impl broker::Agents for EngineAgents {
    fn answer<'a>(
        &'a self,
        manifest: Option<&'a Manifest>,
        call: Call,
    ) -> BoxFuture<'a, Result<AgentsAnswer, WireError>> {
        Box::pin(async move {
            let method = call.method();
            let app = manifest.map_or("shell", |m| m.id.as_str());
            let answer = self.reply(manifest, call).await;
            if let Err(e) = &answer {
                refused(app, method, e);
            }
            answer
        })
    }
}

/// Every refused agents call goes to the engine's log with its code, so a
/// run shows why a call failed even when the app drops the error.
fn refused(app: &str, method: &str, e: &WireError) {
    let code = serde_json::to_value(e.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{:?}", e.code));
    eprintln!(
        "[isonomic-capsule] info: {method} from {app} refused: {code}: {}",
        e.message
    );
}

/// Forward a session's events, and its live deltas between them, as
/// `session_event` and `live_delta` events on one subscription, then
/// `session_ended` when the host ends it.
fn stream(mut sub: Subscription, session: SessionId) -> Starter {
    Box::new(
        move |subscription: u64, outbox: mpsc::Sender<ServerMessage>| {
            tokio::spawn(async move {
                // `None` once the session is purged and every event is out.
                while let Some(update) = sub.next_update().await {
                    let event = match update {
                        Update::Event(event) => Event::SessionEvent(Box::new(event)),
                        Update::Delta(delta) => Event::LiveDelta(delta),
                    };
                    let message = ServerMessage::Event {
                        subscription,
                        event,
                    };
                    if outbox.send(message).await.is_err() {
                        return;
                    }
                }
                // The host ends a subscription only when its session is
                // purged, so the app can tell it from a dropped connection.
                let event = Event::SessionEnded(SessionEnded {
                    session,
                    reason: EndReason::Purged,
                });
                let message = ServerMessage::Event {
                    subscription,
                    event,
                };
                let _ = outbox.send(message).await;
            })
        },
    )
}
