# Broker: status

Newest first.

## 2026-11-15 · Engine: stays headless as apps get their UI through the SDK

- Decision (Omar, through the orchestrator): apps reach Isonomic UI, and later the shared toolkits (an editor and a node-graph toolkit), through the SDK, compiled into the window once. The engine is unaffected: it serves the wire interfaces and knows nothing about windows (rule 9). Its `UI_REF` exists only so cargo can read the SDK workspace's manifests; nothing here builds the UI.

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

## 2026-11-14 (later) · System interface built; local only, not yet on GitHub

**Built**

- `broker`: a Unix socket server speaking the v0 wire format. Version negotiation, then `system.status`, `system.subscribe`, `system.unsubscribe`, `system.logs`, `system.profile` and `system.shutdown`.
- One writer task per connection owns the write half, so frames never interleave. A subscription starts listening before it takes its snapshot, so no change falls between the two. A subscriber that falls behind is resent every service's current status.
- Single instance: an advisory lock on `engine.lock` beside the socket. The kernel drops it when its holder dies, so a crashed engine never blocks the next one and a live one always does. The socket file is user-only.
- `StackSystem`: the adapter that presents the supervisor's stack, the registry and the hardware profile in wire types. The status light is derived from the same readings as the service list.
- `bin/isonomic-capsule`: binds the socket first, logs where each bundled binary resolved and whether it is executable, runs PostgreSQL first-time setup, starts tier 1, logs the cold-start time, serves until `system.shutdown`, SIGTERM or SIGINT, then stops everything in reverse order and removes the socket. A failed start does not end the engine: it keeps serving status.

**Verified**

- Hermetic tests over a real Unix socket with a fake engine, plus one that drives the real supervisor with a stand-in service: a force-kill is seen by a subscriber as starting, then healthy with the restart counted. 58 tests in the engine workspace, 60 consecutive green runs.
- The real binary against a real PostgreSQL 16 on Linux, driven over the socket by a small script: welcome, subscribe during startup, healthy; force-kill of the server gave a `starting` event at once and `healthy` with one restart 6.0 s later; `system.shutdown` was acknowledged, the engine exited 0, no process was left and the socket was gone. A second engine on the same Capsule was refused.

**Deviations**

- The first single-instance check probed the socket by connecting to it. A test showed that is unreliable (a descriptor briefly copied by a starting child keeps a dead socket answering), so it was replaced by the lock before anything shipped.
- The rule for where bundled binaries live (`bundle/` beside the engine, or `ISONOMIC_BUNDLE_DIR`) sits in the binary for now. It moves to the `packaging` crate when that is ported.

**Deferred**

- Sessions and capability tokens: the app model, Milestone 4. Any local process running as the user can call `system` today.
- Recording calls for the Activity tab.
- The previous protocol version: there is only one so far.

## 2026-11-14 · Not started
