# Agents: status

Newest first. The host for agent sessions (`crates/agents`), over the harness host.

## 2026-11-15 · Agents: the `agents` interface served by the harness host

- `SDK_REF` is `21ed50d` (isonomic-sdk #16 and #19: the agents interface and the `agents.` conformance checks). The SDK pins harness-core at the same URL and rev as this repo (`7a73ca6`). `welcome` carries `schema`, the session event schema the host writes.
- The broker hands every `agents` call, after the token and capability checks, to `broker::Agents`; `agents::EngineAgents` implements it over the harness host. Each of the 22 methods maps one to one onto a host call. `sessions.subscribe` replays the durable log after `from_seq`, then live events, as `session_event` and `live_delta` events; only the opening token ends it.
- The host is built from the engine's parts (`agents::start`): the SQLite session log, the token resolver over `broker::Apps`, Tokio for background work, the wall clock, and the system's locale (zone from `/etc/localtime`, offset from the C library, language from `LANG`). Limits and the policy ceiling are the harness defaults. `recover()` seals turns a previous engine left open before the broker serves.
- (session, id) is checked in the adapter for `approvals.decide`, `questions.answer` and `tools.result`, since the host takes the id alone (isonomic-harness #30). The shell's purge names an app and runs as that app.
- Conformance: **28 of 28**, the six `agents.` checks included, against the real agents host with a scripted model. End-to-end tests drive a Chat turn over the socket: the reply streams, the log replays from the store, another app's session answers `not_found`, purge is per app, stop is safe to repeat.
- Deviations:
  - Live deltas do not wake a subscriber in the host (isonomic-harness #32), so a stream also drains them every 40 ms.
  - The engine binary runs the host with no model until the gateway lands: a turn ends with a readable `model_unavailable` error in the log.
  - Client tools are refused and attachments are kept in memory; V1 Chat uses neither.
  - The engine's profiles have no `intel` tier yet, so Intel Macs run under `low`, `standard` or `high` by memory.

## 2026-11-15 · Agents: the token resolver and user-action tokens

- `EngineScopes`, the harness host's `ScopeResolver`: a capability token resolves through `broker::Apps`, exactly as the broker resolves it, to `Caller::app(id, FirstParty)`; an unknown token is `Unauthorized`. Every app the engine grants is first-party; Views and third-party code get their kinds when they arrive.
- User-action tokens: `EngineScopes::mint(caller, approval)` when the shell reports that a person acted. Random (256 bits), single use (spent by the first check, right or wrong), bound to that caller's scope and that approval, and good for 120 s on the engine's injected clock. Kept in memory only.
- `broker::Services.apps` is now shared (`Arc`), so the broker and the agents host resolve the same tokens. harness-host is pinned at the same URL and rev as harness-core.
- Not yet wired: the broker has no method that mints a user-action token; that arrives with the `agents` interface (isonomic-sdk #16).

## 2026-11-15 · Agents: the session log on SQLite (preparation)

- New crate `agents`, planned in `docs/plans/agents.md`: adapters over the harness host's traits, and nothing of the loop or policy.
- Built: harness-log's `SessionStore` over two tables in the registry database. One row per event (session, seq, schema version, payload), appended in one transaction that refuses a gap, an overwrite or another session's event. One registry row per session; deletes only through `purge`. Tested call for call against the harness's `MemoryStore`, and across a reopen.
- harness-core and harness-log come from isonomic-harness at the URL and rev isonomic-sdk #16 pins (`7a73ca6`), so cargo builds one copy of harness-core. CI passes `harness: true`; `HARNESS_DEPLOY_KEY` is visible to this repo.
- Not started, waiting on isonomic-sdk #16: the `agents` methods, `welcome.schema`, and moving `SDK_REF`. Waiting on the model gateway: `ModelClient`.
