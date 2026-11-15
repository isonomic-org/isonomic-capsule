# Engine status

Newest first. This file is the engine's status from now on. The history of each area, moved here from `isonomic-desktop/docs/architecture/<area>/status.md` at that repo's commit `eb30c136`, is in `docs/status/`:

- [Broker](status/engine-broker.md): the socket server and the wire interfaces.
- [Registry](status/engine-registry.md): observed state, grants.
- [Engine core and supervisor](status/engine-supervisor.md): profiles, the lazy tier, the RAM gate.
- [Services](status/engine-services.md): the database, the cache, the vector database.
- [Packaging](status/packaging.md): bundled services and the app bundle.
- [Later modules](status/engine-later.md): reserved crates.

New entries go at the top of this file, naming the area. The spec stays in `isonomic-desktop/docs/architecture/`.

## 2026-11-15 · Gateway: local models through isonomic-inference (SDK request R2)

- New crate `gateway`, pinned to isonomic-inference `4d01814` (`INFERENCE_REF`; CI passes `inference: true`). The broker hands `models` calls to it through `broker::Models`, after the token and capability checks.
- **Catalogue:** the model library's manifest. A model is installed when its file is in `<capsule>/models` at full size. `models.list` shows the installed models the caller may use; `models.info` reports the worker's state.
- **Choosing:** a named model, else the user's default. On a slow machine (an Intel Mac, or the low profile) the default chat model is the smallest installed one, following the inference benchmarks; otherwise the manifest's default if installed. `proto::auth::authorize_model` runs on the model chosen. Not installed answers `model_unavailable`; the wrong kind of model, `unsupported`.
- **Memory:** the engine's profile gate is checked before a worker starts, so `insufficient_memory` carries the gate's own numbers. The same gate is handed to the inference supervisor.
- **Workers:** one llama.cpp server per model, from the bundle, started on first use and unloaded when idle by the inference supervisor, with a per-launch API key. Without the bundled server, streams answer `model_unavailable`.
- **Streams:** text and reasoning chunks as they come, then exactly one chunk with `stop` (`end`, `max_tokens`, `error` with the reason, or `cancelled`). `models.cancel` belongs to the opening token and ends the stream with `stop: cancelled`; a closed connection drops the stream, which stops the server generating. `models.embed` runs on the embeddings model, in input order.
- **Agents:** `agents::GatewayModel` is the harness host's model client over the gateway (`local:default`, `local:smallest` or `local:<id>`). The engine binary runs agent sessions on it.
- **Verified** with the inference testkit's fake model server and a stand-in worker: default and slow-machine choice, filtered list, refusals, the gate's numbers, embeddings, owner-only cancel, and one Chat turn end to end through the broker, the agents host and the gateway, with the reply's text in the session's log.
- Not yet: tools on local models (the provider takes none: `unsupported`), usage numbers (the provider reports none), continuing a reply (`prefill` off), the download methods (isonomic-sdk #26, in review). The llama.cpp server is still a deferred sidecar in `sidecars.json`, so a packaged app does not carry it yet.

## 2026-11-15 · `appdb`: dropped read grants are revoked; every write wakes live queries

- Re-granting an app whose manifest no longer lists a schema takes its read access back: usage, select on the tables there, and the default privilege for tables made later. Found from the role's own grants in the schema ACLs.
- Live queries wake on every committed transaction that wrote, whichever method ran it (a write sent through `sql.query` included) and whichever engine part shares the database. The database decides: a transaction id is assigned only on a write. Reads wake nothing.
- Both checked on the real server (`appdb`'s `real_database` test). The two deviations listed under R5 are closed.

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

## 2026-11-15 · Engine: stays headless as apps get their UI through the SDK

- Decision (Omar, through the orchestrator): apps reach Isonomic UI, and later the shared toolkits (an editor and a node-graph toolkit), through the SDK, compiled into the window once. The engine is unaffected: it serves the wire interfaces and knows nothing about windows (rule 9). Its `UI_REF` exists only so cargo can read the SDK workspace's manifests; nothing here builds the UI.

## 2026-11-15 · Agents: the token resolver and user-action tokens

- `EngineScopes`, the harness host's `ScopeResolver`: a capability token resolves through `broker::Apps`, exactly as the broker resolves it, to `Caller::app(id, FirstParty)`; an unknown token is `Unauthorized`. Every app the engine grants is first-party; Views and third-party code get their kinds when they arrive.
- User-action tokens: `EngineScopes::mint(caller, approval)` when the shell reports that a person acted. Random (256 bits), single use (spent by the first check, right or wrong), bound to that caller's scope and that approval, and good for 120 s on the engine's injected clock. Kept in memory only.
- `broker::Services.apps` is now shared (`Arc`), so the broker and the agents host resolve the same tokens. harness-host is pinned at the same URL and rev as harness-core.
- Not yet wired: the broker has no method that mints a user-action token; that arrives with the `agents` interface (isonomic-sdk #16).

## 2026-11-15 · Broker and `appdb`: app databases (SDK request R5)

- The five `sql` methods. New crate `appdb`: the apps' database on the Capsule's own server, over its Unix socket, in one database (`apps`) created on first use.
- **The roles are the guard.** Each app gets a login role (`app_` and a digest of its id; no superuser, no role or database creation) that owns the schemas its manifest declares and has usage and select on the schemas it lists (current tables, and those their owner creates later). Its statements run on a connection logged in as that role, so no statement can step outside it (`set role` is refused). Nobody but owners reaches `public`. The shell runs as the engine's role in the schema it names. `proto::auth::authorize_schema` picks the schema first.
- Setup is lazy: an app's role and schemas are made before its first statement, and again after each `apps.grant`. A listed schema that does not exist yet is granted once it does. A schema another app owns is refused.
- Each call is one transaction with the search path set for it alone. Placeholders use the extended protocol, so one call is one statement. A batch is one transaction. Values: null, boolean, integers, floats, text, bytes; uuid, json, jsonb, date and timestamps arrive as text; other types (numeric, intervals, arrays) answer `sql_error` asking for a cast. Columns report the database's own type names (`int8`, `text`, `timestamptz`).
- Live queries listen before their first run and run again after every committed write through the interface; the subscriber gets the whole new result only when it changed. A result larger than one frame answers `sql_error`.
- **Verified on a real server:** `appdb`'s `real_database` test and the conformance suite's opt-in run both start PostgreSQL 16 through the engine's own setup and supervisor (Linux, as a non-root user). The conformance suite passes **22 of 22** there. The hermetic CI run uses a stand-in database below the schema rules and also passes 22 of 22.
- Deviations (both closed since): a read-only schema removed from a manifest kept its grant; writes outside this interface did not wake live queries.

## 2026-11-15 · Broker: app events (SDK request R4)

- `events.publish`, `events.subscribe` and `events.unsubscribe`. One broadcast channel per scope (an app's id, or the shell's), shared by every connection, so an app's tabs and windows hear each other. The shell may name an app's scope; `proto::auth` keeps apps in their own.
- Live, at most once: nothing is stored; a subscriber more than 256 events behind misses events instead of slowing the publisher. `data` is passed through untouched and never logged. Bad topics answer `bad_request`.
- The `listening` reply goes out before the first event: the subscriber listens before the reply and forwards only after it.
- Every subscription (system ones too) remembers the token that opened it; only that caller may end it (`unknown_subscription` otherwise). A closed connection ends its subscriptions.
- Conformance: 21 of 22. Only `sql.schemas` (R5) remains.

## 2026-11-15 · Broker: blob storage (SDK request R3)

- The six `blobs` methods. Each blob is a file in `<capsule>/blobs` (user-only), named by a random handle, with a row in the registry database: owner, content type, size, bytes written.
- Persist, then answer: a write's bytes are synced to disk before the row counts them, and the row is committed before the reply, so a client that saw `written: n` resumes at `n` after a restart. Bytes past the count (a crash mid-write) are dropped at the next write.
- Limits: the manifest's `max_bytes` (through `proto::auth`), then the engine's own 64 MiB, then `CHUNK_BYTES` per call and the declared size per write. Another app's handle answers `unknown_blob`; the shell reaches every blob. Blob calls run off the async runtime.
- Conformance: 17 of 22. The blobs checks and `wire.unknown_fields` now pass; events (R4) and sql (R5) remain.

## 2026-11-15 · Agents: the session log on SQLite (preparation)

- New crate `agents`, planned in `docs/plans/agents.md`: adapters over the harness host's traits, and nothing of the loop or policy.
- Built: harness-log's `SessionStore` over two tables in the registry database. One row per event (session, seq, schema version, payload), appended in one transaction that refuses a gap, an overwrite or another session's event. One registry row per session; deletes only through `purge`. Tested call for call against the harness's `MemoryStore`, and across a reopen.
- harness-core and harness-log come from isonomic-harness at the URL and rev isonomic-sdk #16 pins (`7a73ca6`), so cargo builds one copy of harness-core. CI passes `harness: true`; `HARNESS_DEPLOY_KEY` is visible to this repo.
- Not started, waiting on isonomic-sdk #16: the `agents` methods, `welcome.schema`, and moving `SDK_REF`. Waiting on the model gateway: `ModelClient`.

## 2026-11-15 · Broker: capability tokens and `apps.grant` (SDK request R1)

- `apps.grant` (shell only) validates the manifest (`invalid_manifest`), records the approval and the token in the registry, then answers `granted { app, token }`.
- A token is `tok-` and an HMAC-SHA256 of the app id and its approved capabilities, under a 32-byte secret in `secrets/token.key` (user-only, created once from the system's random source, never replaced). The same approval gives the same token after a restart; the registry stores only a SHA-256 digest of each token.
- Granting an app again replaces its manifest and keeps every earlier token valid, on the new manifest.
- Every request: no token is the shell; a token is looked up (`unknown_token` if never issued), then `proto::auth::authorize` runs before any work. Tokens appear in no log line or error message.
- Conformance (isonomic-sdk `crates/conformance`, run by `crates/broker/tests/conformance.rs` against a test engine in a temporary Capsule): 12 of 22 checks pass, every `wire.` and `tokens.` check except `wire.unknown_fields`, which probes with `blobs.info` and waits on R3. The blobs, events, subscriptions and sql checks wait on R3 to R5. The test fails if a listed check starts passing, so the list only shrinks.
- CI checks out isonomic-ui at `4c912f6` (`UI_REF`): the SDK workspace names it, so cargo needs it to read manifests. Nothing here builds it.

## 2026-11-15 · Broker: the SDK pin moves to isonomic-sdk main

- `SDK_REF` is `7c488c2` (isonomic-sdk #15, `system.retry`). The broker builds against it.
- Requests are read with `io::read_frame` and decoded leniently: a newer window's unknown method is answered `unknown_method` with its own id, not id 0; a frame that is not a request is answered with id 0. Errors are built with `WireError::new`.
- A request with a token is refused `unknown_token` (no app has been granted one yet; tokens arrive with R1). The message never repeats the token.
- Methods the engine does not serve yet (`system.retry`, `apps`, `models`, `blobs`, `events`, `sql`) are answered `unknown_method`.
