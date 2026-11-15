# Engine status

Newest first. This file is the engine's status from now on. The history of each area, moved here from `isonomic-desktop/docs/architecture/<area>/status.md` at that repo's commit `eb30c136`, is in `docs/status/`:

- [Broker](status/engine-broker.md): the socket server and the wire interfaces.
- [Registry](status/engine-registry.md): observed state, grants.
- [Engine core and supervisor](status/engine-supervisor.md): profiles, the lazy tier, the RAM gate.
- [Services](status/engine-services.md): the database, the cache, the vector database.
- [Packaging](status/packaging.md): bundled services and the app bundle.
- [Later modules](status/engine-later.md): reserved crates.

New entries go at the top of this file, naming the area. The spec stays in `isonomic-desktop/docs/architecture/`.

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
