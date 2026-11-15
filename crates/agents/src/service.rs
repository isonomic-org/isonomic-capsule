//! The `agents` interface served by the harness host: each method maps one
//! to one onto a host call. The broker has checked the token and the
//! capability; the host decides what each caller reaches.
//!
//! Two things the host does not do yet, done here:
//! - Approval, question and call ids are minted per session, so the wire
//!   names the session as well. The host takes only the id (isonomic-harness
//!   issue #30), so the adapter checks the (session, id) pair first.
//! - Live deltas are queued for a subscriber without waking it, so a
//!   stream drains them every `DELTA_TICK` as well as before each event.

use std::sync::Arc;
use std::time::Duration;

use broker::agents::{AgentsAnswer, BoxFuture, Starter};
use harness_core::{CallId, Event as LogEvent, SessionId};
use harness_host::{
    AnswerInput, AppKind, Caller, ClientOutcome, CreateOptions, Host, HostError, Input,
    Subscription, UserActionToken,
};
use harness_log::SessionStore;
use isonomic_proto::{Call, ErrorCode, Event, Manifest, Reply, ServerMessage, WireError};
use tokio::sync::mpsc;

use crate::convert;

/// How long a streaming reply's text may wait before it is sent.
pub const DELTA_TICK: Duration = Duration::from_millis(40);

pub struct EngineAgents {
    host: Host,
    store: Arc<dyn SessionStore>,
}

fn caller(manifest: Option<&Manifest>) -> Caller {
    match manifest {
        None => Caller::shell(),
        Some(m) => Caller::app(&m.id, AppKind::FirstParty),
    }
}

fn not_found() -> WireError {
    convert::error(HostError::NotFound)
}

fn done(result: Result<(), HostError>) -> Result<Reply, WireError> {
    result.map(|()| Reply::Done {}).map_err(convert::error)
}

impl EngineAgents {
    /// Serve `host`, whose log is `store`. Call [`Host::recover`] before
    /// the broker serves.
    pub fn new(host: Host, store: Arc<dyn SessionStore>) -> Self {
        Self { host, store }
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Whether `call` is a client tool call of `session`, per its log.
    async fn has_call(&self, session: &SessionId, call: &CallId) -> Result<bool, WireError> {
        let mut after = 0;
        loop {
            let page = match self.store.read(session, after, 512).await {
                Ok(page) => page,
                Err(_) => return Ok(false),
            };
            let Some(last) = page.last() else {
                return Ok(false);
            };
            after = last.seq;
            let found = page
                .iter()
                .any(|e| matches!(&e.event, LogEvent::ToolCall(c) if &c.call == call));
            if found {
                return Ok(true);
            }
        }
    }

    async fn reply(&self, who: &Caller, call: Call) -> Result<AgentsAnswer, WireError> {
        let host = &self.host;
        let err = convert::error;
        let reply = match call {
            Call::SessionsCreate(c) => {
                let opts = CreateOptions {
                    title: c.title,
                    location_opt_in: c.location_opt_in,
                    limits: None,
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
                // `proto::auth` lets only the shell name an app, and refuses
                // the shell's unnamed purge.
                let target = match p.app {
                    Some(app) => Caller::app(app.as_str(), AppKind::FirstParty),
                    None => who.clone(),
                };
                Reply::Purged(convert::purge(host.purge(&target).await.map_err(err)?))
            }
            Call::SessionsSubscribe(s) => {
                let sub = host.subscribe(who, &s.session, s.from_seq).await;
                return Ok(AgentsAnswer::Stream(stream(sub.map_err(err)?)));
            }
            Call::ApprovalsList(a) => {
                let all = host.list_approvals(who).await.map_err(err)?;
                let wanted = all
                    .into_iter()
                    .filter(|p| a.app.as_ref().is_none_or(|app| &p.owner == app));
                Reply::Approvals(wanted.map(convert::approval).collect())
            }
            Call::ApprovalsDecide(d) => {
                let pending = host.list_approvals(who).await.map_err(err)?;
                let ours = pending
                    .iter()
                    .any(|p| p.session == d.session && p.request.approval == d.approval);
                if !ours {
                    return Err(not_found());
                }
                let token = d.user_action.map(UserActionToken);
                let decision = convert::decision(d.decision);
                done(
                    host.decide_approval(who, &d.approval, decision, token)
                        .await,
                )?
            }
            Call::QuestionsAnswer(q) => {
                let snapshot = host.open_session(who, &q.session).await.map_err(err)?;
                if !snapshot.questions.contains(&q.question) {
                    return Err(not_found());
                }
                let answer = AnswerInput {
                    selected: q.selected,
                    text: q.text,
                    skipped: q.skipped,
                };
                done(host.answer_question(who, &q.question, answer).await)?
            }
            Call::ToolsRegisterClient(t) => {
                done(host.register_client_tools(who, &t.session, t.specs).await)?
            }
            Call::ToolsResult(r) => {
                // The caller must reach the session, and the call must be its.
                host.open_session(who, &r.session).await.map_err(err)?;
                if !self.has_call(&r.session, &r.call).await? {
                    return Err(not_found());
                }
                let outcome = match r.error {
                    Some(error) => ClientOutcome::Error(error, r.content),
                    None => ClientOutcome::Ok(r.content),
                };
                done(host.client_tool_result(who, &r.call, outcome).await)?
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
                Reply::Artifact(Box::new(convert::artifact(version.map_err(err)?)))
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
        Box::pin(async move { self.reply(&caller(manifest), call).await })
    }
}

/// Forward a session's events, and its live deltas between them, as
/// `session_event` and `live_delta` events on one subscription.
fn stream(mut sub: Subscription) -> Starter {
    Box::new(
        move |subscription: u64, outbox: mpsc::Sender<ServerMessage>| {
            tokio::spawn(async move {
                let send = |event: Event| ServerMessage::Event {
                    subscription,
                    event,
                };
                let mut tick = tokio::time::interval(DELTA_TICK);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    let next = tokio::select! {
                        next = sub.next() => Some(next),
                        _ = tick.tick() => None,
                    };
                    // Deltas first: they are older than the event that woke us.
                    for delta in sub.deltas() {
                        if outbox.send(send(Event::LiveDelta(delta))).await.is_err() {
                            return;
                        }
                    }
                    match next {
                        None => continue,
                        Some(None) => return, // purged
                        Some(Some(event)) => {
                            let event = Event::SessionEvent(Box::new(event));
                            if outbox.send(send(event)).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            })
        },
    )
}
