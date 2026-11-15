# Requests to the engine

What the window's apps need from the engine, filed by the SDK session. Newest first. Each request names the SDK commit that brings its types. The in-memory engine in `isonomic-testkit` already behaves as described unless an item says otherwise, so the SDK's tests are the reference. When a request is done, add the engine commit that closes it.

## R6 · Run the conformance checks

**Status:** open. **`SDK_REF`:** `f2545b20ee0faea12f0bece91c243c086ba806c1` (isonomic-sdk #11). **Spec:** isonomic-sdk `docs/conformance.md`.

1. **Add `isonomic-conformance`** as a dev-dependency at the same `SDK_REF`, start a test engine in a temporary Capsule directory, and run `isonomic_conformance::run(unix_connector(socket))` against it in CI. Each failure names the rule it broke.
2. **Grow into it.** Until every interface is in, run a subset with `run_only`: the `wire.` and `tokens.` checks once R1 lands, then each interface's group as it does (`blobs.`, `events.`, `sql.`).
3. **Never against a user's Capsule.** The checks grant test apps (ids under `com.isonomic.conformance`), store and delete small blobs, and end by sending `system.shutdown` with an app's token, which must be refused.

**Check:** the SDK's `crates/conformance/tests/testkit.rs`: the testkit passes all 22 checks, and an engine that ignores tokens fails every token, privacy and scope check.

## R5 · App databases (`sql` interface)

**Status:** open. **`SDK_REF`:** `02b838a8526423afeb337631f716d105978a1bb5` (isonomic-sdk #10). **Spec:** isonomic-sdk `docs/interfaces/sql.md`; every shape is a fixture in `crates/proto/fixtures/v0/sql.jsonl`.

1. **Five methods:** `sql.query`, `sql.execute`, `sql.batch`, `sql.subscribe`, `sql.unsubscribe`, on the Capsule's database. `proto::auth::authorize` already refuses an app without the `sql` capability.
2. **Schemas and roles.** On `apps.grant`, create each schema the manifest declares, owned by a role for that app, and give that role read-only access to the schemas listed in `capabilities.sql.schemas`. Run each call as the app's role, in the schema `proto::auth::authorize_schema(manifest, schema, write)` returns (`write` is true for `execute` and `batch`). The roles are the real guard: a write sent through `sql.query` to a listed schema is refused by the database (`sql_error`). The shell runs in the schema it names.
3. **Values.** Parameters are positional (`$1`, `$2`, …). `SqlValue` maps to null, boolean, 64-bit integer, double, text and bytes; each column reports the database's own type name.
4. **A batch is one transaction:** every statement or none, with one count per statement.
5. **Live queries.** Reply `live_query { subscription, rows }` with the result now, then send `rows_changed` with the whole new result after each committed write that changes it. Re-running the query when a write touches a table it reads is enough; send nothing when the result is unchanged. Only the token that opened it can end it (`unknown_subscription` otherwise, as R2 item 5); a closed connection ends it.
6. **Errors.** A statement the database refuses answers `sql_error` with the database's message. Keep a reply within `MAX_FRAME_BYTES`: a larger result answers `sql_error`, so apps page with `limit`.

**Check:** the SDK's `crates/testkit/tests/sql.rs` (the testkit's database is scripted; it runs no SQL) and the conformance check `sql.schemas`.

## R4 · App events (`events` interface)

**Status:** open. **`SDK_REF`:** `f8b3f0dc066af8a9c27ecb61f89ce9d3bbe178b0` (isonomic-sdk #9). **Spec:** isonomic-sdk `docs/interfaces/events.md`; every shape is a fixture in `crates/proto/fixtures/v0/events.jsonl`.

1. **Three methods:** `events.publish`, `events.subscribe`, `events.unsubscribe`. No capability: `proto::auth::authorize` already keeps an app in its own scope and lets only the shell name another app's (`app`).
2. **One scope per app**, shared by every connection, so an app's tabs and windows hear each other. A subscriber gets `app_event { topic, data, sender }` for each publish in its scope whose topic it listens to (`proto::events::matches`; no topics means all). Refuse a topic that fails `proto::events::valid_topic` with `bad_request`.
3. **Live, at most once.** Nothing is stored or replayed. Answer `published {}` once the event is handed to current subscribers; drop events for a subscriber that falls behind rather than slow the publisher.
4. **Unsubscribe belongs to the token that subscribed** (`unknown_subscription` otherwise); a closed connection ends its subscriptions.
5. **`data` is the app's own JSON.** Pass it through untouched and keep it out of logs.

**Check:** the SDK's `crates/testkit/tests/events.rs` and the conformance checks `events.round_trip`, `events.scoped`, `events.shell_reaches_app` and `subscriptions.owner_only`.

## R3 · Blob storage (`blobs` interface)

**Status:** done (engine PR #30; see `docs/status.md`). **`SDK_REF`:** `faddbf121ca2ab403140cc5e529c093fa996fbf3` (isonomic-sdk #8). **Spec:** isonomic-sdk `docs/interfaces/blobs.md`; every shape is a fixture in `crates/proto/fixtures/v0/blobs.jsonl`.

1. **Six methods:** `blobs.put`, `blobs.create`, `blobs.write`, `blobs.get`, `blobs.info`, `blobs.delete`, over the engine's content store. Bytes are base64 on the wire, at most `CHUNK_BYTES` (512 KiB) per call.
2. **Limits.** `proto::auth::authorize` already refuses apps without the `blobs` capability and anything over the manifest's `max_bytes`. Apply the engine's own limit on top with `auth::too_large(size, limit)`, and refuse a chunk over `CHUNK_BYTES`.
3. **Ownership.** A blob belongs to the app that stored it. Another app's handle answers `unknown_blob`, exactly as if it did not exist; the shell reads every blob. Handles are opaque and never derived from content.
4. **Order.** `blobs.create` reserves the size; each `blobs.write` must start at the bytes written so far (`bad_request` otherwise) and may not pass the size (`too_large`). `blobs.get` before the last byte answers `incomplete`; afterwards it returns at most `CHUNK_BYTES` from `offset`.
5. **Persist, then answer.** A write is acknowledged only once its bytes are durable, so a client that saw `written: n` can resume at `n` after a restart.

**Check:** the SDK's `crates/testkit/tests/blobs.rs`: small and chunked round trips, ranges, ownership, both limits, half-written reads, out-of-order writes, and the capability check on every method.

## R2 · The model gateway (`models` interface)

**Status:** open. **`SDK_REF`:** `2d7b9760323a9d5587712add4d8fe07a094fca15` (isonomic-sdk #4). **Spec:** isonomic-sdk `docs/interfaces/models.md`; every shape is a fixture in `crates/proto/fixtures/v0/models.jsonl`.

**Moving `SDK_REF` to it:** `WireError` gains a `memory` field, so build errors with `WireError::new(code, message)` or `WireError::memory(refusal)`. `Call`, `Reply` and `ServerMessage` lose `Eq` (temperatures and vectors are floats).

1. **Five methods:** `models.list`, `models.info`, `models.stream`, `models.cancel`, `models.embed`, on top of the model gateway.
2. **Check, then check again.** Call `proto::auth::authorize(caller, &call)` first; it refuses a model the manifest does not name. A call that leaves `model` out runs on the user's default chat (or embeddings) model: choose it, then call `proto::auth::authorize_model(caller, &model, remote)` before loading anything. The reply names the model chosen.
3. **`models.list` is filtered** to the models the caller may use (`ModelAccess::permits` for an app; the shell sees all), so an app's picker never offers a model it would be refused.
4. **Order on a stream:** send the `stream { subscription, model }` reply before the first `model_chunk` event. Every stream ends with exactly one chunk that has `stop`, and nothing follows it. `usage`, when known, rides on that last chunk.
5. **Cancel belongs to the token that opened the stream.** Apps share the window's connection, so `models.cancel` from any other caller is answered `unknown_subscription`, as for a stream that has ended. A cancelled stream ends with a `stop: cancelled` chunk; a closed connection cancels all of its streams.
6. **The memory gate refuses before a stream opens:** `insufficient_memory`, with `memory { required_bytes, available_bytes, reason }` filled in from the gate's own numbers, so the app can say why and offer a smaller or remote model.
7. **Errors:** `unknown_model` (no such model, or no default of that kind); `unsupported` (embeddings from a chat model, say); `model_unavailable` (the worker is degraded or crashed, also as the `error` on a `stop: error` chunk mid-stream); `remote_disabled` (a remote model while the user's switch is off).
8. **The remote switch is the user's, and off by default.** Turning it off cuts running remote streams with `stop: cancelled`. (The testkit does not model this cut yet: its switch only refuses new calls.)

**Check:** the SDK's `crates/testkit/tests/models.rs` lists the behaviour case by case: every chunk kind in order, cancel and drop-to-cancel, cross-app cancel refused, the memory refusal's numbers, failure before and during a stream, recovery, embeddings in input order, and the token check on every method.

## R1 · Capability tokens, `apps.grant` and tolerant decoding

**Status:** done (engine PRs #26 and #27; see `docs/status.md`). **`SDK_REF`:** `32c09ffa29dd11ea0a25c072978b31f6c2b3b794` (isonomic-sdk #3). **Spec:** isonomic-sdk `docs/interfaces/apps.md`, `docs/interfaces/system.md` and the changelog.

**Moving `SDK_REF` to it** breaks the broker in four places, all in `crates/broker/src/conn.rs`:

- `ClientMessage::Request { id, call }` has a `token` field: `{ id, token, call }`.
- `Call` is `#[non_exhaustive]`: the `match call` needs a `_` arm, answered `unknown_method`.
- Error literals: use `WireError::new(code, message)` (R2 adds a field).
- Reading: replace `read_message::<_, ClientMessage>` with `io::read_frame` then `lenient::decode_client`. Answer `Rejected { id, error }` with that id (an unknown method from a newer window is answered `unknown_method` with its own id, not id 0); answer `Garbage(error)` with id 0.

Then:

1. **Who is calling.** No token: `Caller::Shell`, the window on its own socket. A token: look up the app's approved manifest and build `Caller::App { id, capabilities }`. A token no app holds: `unknown_token`. Then `proto::auth::authorize(caller, &call)` on every request, before any work; its error is the answer. It already refuses `system` and `apps` to every app token.
2. **`apps.grant { manifest }`** (shell only): `Manifest::validate()`, refusing with `invalid_manifest`; record the approval in the registry, then reply `granted { app, token }`. Granting the same app again keeps its earlier tokens valid, so open tabs keep working.
3. **Tokens survive a restart.** Derive each token from the approved manifest with a secret kept in the Capsule directory (a keyed hash of the app id and its approved capabilities, say), so the same approval yields the same token after a restart and the window's open tabs keep working. The secret never leaves the engine.
4. **Tokens are never logged.** `Token`'s `Debug` already prints `Token(..)`; keep them out of log lines and error messages too.

**Check:** the SDK's `crates/testkit/tests/tokens.rs` and `tolerance.rs`: tokenless calls are the shell's, app tokens are refused `system` and `apps` and anything their manifest lacks, unknown and revoked tokens are refused, and a newer client's unknown method is answered with its own id.
