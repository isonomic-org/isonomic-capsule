# Engine status

Newest first. This file is the engine's status from now on. The history of each area, moved here from `isonomic-desktop/docs/architecture/<area>/status.md` at that repo's commit `eb30c136`, is in `docs/status/`:

- [Broker](status/engine-broker.md): the socket server and the wire interfaces (tokens, blobs, events, sql).
- [Agents](status/agents.md): agent sessions over the harness host.
- [Registry](status/engine-registry.md): observed state, grants.
- [Engine core and supervisor](status/engine-supervisor.md): profiles, the lazy tier, the RAM gate.
- [Services](status/engine-services.md): the database, the cache, the vector database.
- [Packaging](status/packaging.md): bundled services and the app bundle.
- [Later modules](status/engine-later.md): reserved crates.

New entries go at the top of this file, naming the area; older ones move to their area's file when this one nears 20 KB. The spec stays in `isonomic-desktop/docs/architecture/`.

## 2026-11-15 · Gateway: model downloads and the first model (isonomic-sdk #26)

- `SDK_REF` is isonomic-sdk #26's head (`af8e023`) until it merges; the pin moves to its merge commit then.
- `models.download`, `models.download_cancel` and `models.recommended`, all shell only (`proto::auth`).
- **The engine owns each download.** It runs in the gateway with the model library's downloader: the file arrives as `<name>.part`, its SHA-256 is checked against the manifest, then it is renamed in, so a model is installed only whole. A second `models.download` attaches to the running one and is first sent where it stands; an installed model gets one final `installed` event. The event with an `outcome` (`installed`, `failed` with the reason, `cancelled`) is the last. Sizes are absent when unknown, never 0.
- **Cancel takes the model** and ends every follower `cancelled`; with nothing running it answers `not_downloading`. A cancelled download leaves at most a `.part` file, which the next attempt replaces.
- **The first model:** below 16 GiB of total memory, `insufficient_memory` with the machine's total as `available_bytes`. Otherwise the smallest chat model for an Intel Mac or the `low` profile, else the manifest's default (Qwen3.5 4B today). An unknown profile is `bad_request`.
- `models.info` reports `downloading` and `not_installed`; streams and embeddings refuse a model that is not installed (`model_unavailable`).
- Downloads come only from the manifest's https sources. Tests inject a loopback source (`Gateway::with_source`) and serve the file from the test itself, held halfway for the attach and cancel cases.

## 2026-11-15 · Packaging: the llama.cpp server ships

- `llama-server` is no longer deferred in `sidecars.json`, so the app bundles it and the gateway finds it at `sidecars/llama-server-<triple>/llama-server`.
- Pinned to llama.cpp `b12387`, the build isonomic-inference smoke-tested and benchmarked on the Intel Mac (the earlier pin, `b10331`, was never run against the models in the manifest). Hashes recorded from the release archives: arm64 `a654e602…`, x86_64 `624b91b4…`; 17 MB and 13 MB compressed. The whole directory ships (the server needs its `libggml*` and `libllama*` libraries beside it).

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
