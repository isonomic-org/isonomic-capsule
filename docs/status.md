# Engine status

Newest first. This file is the engine's status from now on. The history of each area, moved here from `isonomic-desktop/docs/architecture/<area>/status.md` at that repo's commit `eb30c136`, is in `docs/status/`:

- [Broker](status/engine-broker.md): the socket server and the wire interfaces.
- [Registry](status/engine-registry.md): observed state, grants.
- [Engine core and supervisor](status/engine-supervisor.md): profiles, the lazy tier, the RAM gate.
- [Services](status/engine-services.md): the database, the cache, the vector database.
- [Packaging](status/packaging.md): bundled services and the app bundle.
- [Later modules](status/engine-later.md): reserved crates.

New entries go at the top of this file, naming the area. The spec stays in `isonomic-desktop/docs/architecture/`.

## 2026-11-15 · Broker: the SDK pin moves to isonomic-sdk main

- `SDK_REF` is `7c488c2` (isonomic-sdk #15, `system.retry`). The broker builds against it.
- Requests are read with `io::read_frame` and decoded leniently: a newer window's unknown method is answered `unknown_method` with its own id, not id 0; a frame that is not a request is answered with id 0. Errors are built with `WireError::new`.
- A request with a token is refused `unknown_token` (no app has been granted one yet; tokens arrive with R1). The message never repeats the token.
- Methods the engine does not serve yet (`system.retry`, `apps`, `models`, `blobs`, `events`, `sql`) are answered `unknown_method`.
