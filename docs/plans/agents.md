# Plan: the agents adapter (`crates/agents`)

Status: preparation. Nothing here depends on isonomic-sdk #16 until the orchestrator says it has merged; #16 is being revised (`approvals.decide`, `questions.answer` and `tools.result` gain `session`; `welcome` gains a required `schema`), so no code is built against its current shapes.

## What it is

A crate that hosts harness sessions inside the engine and serves the SDK's `agents` interface over the broker. It holds adapters over `harness-host`'s traits only: no loop, registry or policy code. The loop, policy, approvals and event ordering are the harness host's; the engine supplies storage, models, tokens, clients and time.

## Trait by trait

| `harness-host` trait | Engine part | Depends on |
| --- | --- | --- |
| `SessionStore` | A table in the registry database: one row per event (session, seq, schema version, payload), appends in one transaction that refuse a gap or an overwrite; session metadata rows beside it. Deletes only through `purge`. | harness-core |
| `ScopeResolver` | `broker::Apps`: a token resolves to its app's approved manifest; no token is the shell. User-action tokens are minted by the broker when the shell reports a person's action, kept in memory, single use, per approval. | R1 (done) |
| `BlobStore` | The engine's content store, owned per app and session. | R3 (blobs) |
| `ModelClient` | The model gateway: `ModelRequest` mapped onto the inference protocol's conversation type, streamed through the local provider, under the RAM gate. `caps` from the model library's manifest. | Item 3 (blocked on CI access to isonomic-inference) |
| `ClientToolChannel` | The broker: a `client` tool call is sent as an event to the connection of the app that registered it; `is_connected` follows that app's live connections. `client_connected` / `client_disconnected` are called from the connection loop. | SDK #16 (events) |
| `Spawner`, `Clock` | Tokio, and the engine's injected clock (tests use a manual clock). | — |
| `Locale` | Date, time zone and language from the engine's environment; location only on opt-in. | — |
| `Executor` | The host's in-process default for now. | — |

Engine lifecycle: `Host::new(Deps, HostConfig)` at start, `recover()` before the broker serves, `shutdown()` before the services stop. `HostConfig.profile` is the engine's chosen hardware profile.

## Dependencies and CI

- `harness-host` and `harness-core` come from isonomic-harness. The crate must declare them with the identical URL and rev the SDK uses (`ssh://git@github.com/isonomic-org/isonomic-harness.git`, rev as pinned in the SDK's workspace `Cargo.toml`), or cargo builds two copies of harness-core and the types do not match.
- CI passes `harness: true` to the shared workflow; it needs the `HARNESS_DEPLOY_KEY` secret visible to this repo.

## Order of work

1. `SessionStore` on SQLite. **Done.**
2. `ScopeResolver` over `broker::Apps`, and user-action tokens. **Done.**
3. Once #16 merges and the orchestrator says so: move `SDK_REF`, send `welcome.schema`, serve the 22 `agents` methods, run the `agents.` conformance group.
4. `ModelClient` once the gateway exists; then one real Chat turn end to end.
