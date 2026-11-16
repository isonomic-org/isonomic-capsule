# Gateway status

Newest first. The model gateway: local models through isonomic-inference, the catalogue, downloads and the chat workers. Moved here from `docs/status.md`.

## 2026-11-16 · Gateway and agents: the model picker and the agents follow-ups (isonomic-sdk #30, harness #41 and #42)

- `models.list` and `models.info` mark the model `models.recommended` names for this machine (`ModelInfo.recommended`).
- `sessions.create` takes a `model`: trimmed, at most 128 characters (`agents::MAX_MODEL_NAME`), blank means the preset's own, then checked with the `models.stream` rule (`unknown_model`, `model_unavailable`, `unsupported`, and `capability_denied` from `proto::auth`), and passed to the host's create-time override.
- A model name the gateway does not know fails the turn with a reason ("The model … is not available on this computer.") rather than falling back to another model.
- Chat preset version 2 with no engine tools offered: the persona takes the most likely reading instead of asking, and the tools paragraph is gone. Measured against llama.cpp `b12387` with this exact prompt, 24 replies each: Qwen3.5 0.8B refused or asked to clarify 1 in 24 (about 1 in 6 before) but still invented places in most replies; Qwen3.5 2B refused none and named real Lisbon places in all 24.
- `SessionSummary.model` reports the picked model (absent for the preset's own).
- **Model rules, as the SDK has them:** a name is checked with `proto::agents::model_name`, so a blank name is now `bad_request` instead of meaning the preset's own model. The app's manifest is checked before the model is looked up (`capability_denied` before `unknown_model`), so an app learns nothing about models it may not use. Every model a preset the app stored names is held to the same rule: its reply model (unless the session names its own) and its title model, which runs either way. Built-in presets, and the engine's own `local:default` and `local:smallest`, are not.
- **Streams end with `session_ended { reason: purged }`** when the host ends a subscription, so an app can tell a purge from a dropped connection.
- **`approvals.user_action`** (shell only) mints the token with the engine's own resolver, the one the host verifies decisions with; it holds for the app named, that session and that approval.
- **`artifacts.get` comes in chunks** of at most `CHUNK_BYTES` from `offset` (fewer with `length`). Reading at or past the end of a body that is not empty is `bad_request`.
- Pins: isonomic-sdk `12ddf2f` (main with #30), harness `9a3809f`. Conformance: 30 of 30, with `agents.model_rules` and `agents.session_ended`.

## 2026-11-16 · Gateway and agents: later turns reuse the conversation's cache

- **Why the second turn was slow** (27.5 s to first text on the Intel Mac, against 7.1 s for the first): every turn read the previous reply again from scratch. Found from llama.cpp `b12387`'s own slot logs with Qwen3.5 2B, through the engine's gateway and agents host (`crates/agents/tests/real_model.rs`). There were two causes:
  - **The title step evicted the conversation.** The worker had one slot, and the title prompt differs from the conversation's, so the next turn found the title's cache.
  - **The model's chat template drops the empty thinking block** (`<think>\n\n</think>`) from every reply before the latest request. The model wrote the reply after that block, so the cached tokens stop matching at the start of the previous reply, even on a slot of its own. This is the larger cost: on a slow machine it grows with every long answer.
- **Chat workers now run two slots of 8192 tokens** (`-np 2 -c 16384`): the conversation keeps one and the title takes the other. This costs about 116 MB more resident memory with the 2B model (2.73 → 2.84 GB).
- **Qwen3.5 models get their own template with one line changed** (`crates/gateway/templates/history-kept.jinja`, written into `<models>/templates/` and passed with `--chat-template-file`): earlier replies keep the empty thinking block, as when they were written. The template is checked identical in the 0.8B and 2B models. Other models keep their own template.
- **Measured here** (4 cores, 2B model, three turns of a day plan, the title running after turn 1), first text for turns 2 and 3:
  - before: 3.1 s and 2.6 s, reading 597 and 497 tokens again;
  - two slots only: 2.9 s and 2.8 s;
  - two slots and the template: 0.36 s and 0.37 s, reading only the new 23–25 tokens (the slot keeps 100%).
- **`ModelClient::caps` reports the real context:** the model's window cut to the worker's slot (8192), not the catalogue's 262144. Compaction works from this number.
- **Refused agents calls are logged** with their method, app and code (`info: sessions.continue from com.isonomic.chat refused: continue_refused: …`).
- **Continue after a stop on the first token** was checked against the real server, and the engine does not refuse it. With a one-character first token ("-", "#", "7") or a five-character one ("Hello", "Under"), the continued turn ends `done`. A stop before any text leaves nothing to continue, and the engine says so (`continue_refused`, "There is no partial reply to continue.").

## 2026-11-15 · Gateway: Qwen3.5 2B is the chat model on slow machines

- On an Intel Mac or the low profile, the default chat model is Qwen3.5 2B when it is installed (`gateway::SLOW_CHAT_MODEL`), else the smallest installed one; `models.recommended` names Qwen3.5 2B there. The 0.8B model is faster but invents facts (see the entry on small models below); it stays for titles, which ask for `local:smallest`.
- Other machines keep the manifest's default (Qwen3.5 4B).
- Measured on the 2020 Intel MacBook Pro (i7-8850H, 16 GB, lightly loaded), three turns: model load 5.3 s; first text 1.4–2.0 s; 7.7–16.1 tok/s; every reply named real places. The 0.8B model answered first in 1.0–1.5 s there, with nonsense.

## 2026-11-15 · Gateway and agents: groundwork for the model picker; no engine tools for plain chat

- `Gateway::recommended_id()`: the model `models.recommended` names for this machine's own profile (none below the memory floor), for the picker's `recommended` mark once the wire carries it.
- `Gateway::check_session_model()`: the rule `models.stream` applies, for `sessions.create` naming a model once the wire and the host carry it: the app's `models` capability must allow it (`capability_denied`), it must exist (`unknown_model`), be installed (`model_unavailable`) and chat (`unsupported`).
- `agents::Parts::engine_tools` (off in the engine): the harness's engine tools are no longer offered to sessions. Local models take no tools, and with none offered the chat preset's next version leaves its tools and approvals paragraph out of the prompt.

## 2026-11-15 · Agents and gateway: a better first answer from small models; timed turns

- Why the 0.8B model refused "plan one day in Lisbon" on the Intel Mac, found by replaying the exact request against the same llama.cpp build and model here (4 cores): the request context ("Current date: …") arrived glued to the person's words in the user turn, and the persona says to ask when a request is unclear. Mostly, though, it is the model: in 36 replies the 0.8B model attempted the plan five times in six but invented places that are not in Lisbon, and refused or asked to clarify about one time in six, wherever the context sat. Qwen3.5 2B named real places in every reply and refused once in 24, never once the context moved.
- `GatewayModel` now puts the latest request context at the end of the system message. Reasoning was already off on Intel Macs (the provider's policy).
- Chat workers start with the Qwen3.5 model cards' sampling without thinking (`--temp 0.7 --top-p 0.8 --top-k 20 --min-p 0`; the server's own defaults are 0.8, 0.95, 40, 0.05), checked to take effect on `b12387`. No clear effect on refusals in these trials.
- Turn usage now counts the server's streamed chunks as output tokens (about one token each; the provider reports no usage).
- `one_turn` times the model load apart (a 1-token stream), then N warm turns: first text, total, tokens, tokens per second. Run here against the real engine binary, llama.cpp `b12387` and Qwen3.5 0.8B: load 5.1 s, first text 0.3–0.5 s, 23–28 tok/s. `docs/turn-check.md` has the corrected command (the engine is its own build) and the results.

## 2026-11-15 · Gateway: model downloads and the first model (isonomic-sdk #26)

- `SDK_REF` is `de3e0da`, isonomic-sdk main with #26 merged.
- `models.download`, `models.download_cancel` and `models.recommended`, all shell only (`proto::auth`).
- **The engine owns each download.** It runs in the gateway with the model library's downloader: the file arrives as `<name>.part`, its SHA-256 is checked against the manifest, then it is renamed in, so a model is installed only whole. A second `models.download` attaches to the running one and is first sent where it stands; an installed model gets one final `installed` event. The event with an `outcome` (`installed`, `failed` with the reason, `cancelled`) is the last. Sizes are absent when unknown, never 0.
- **Cancel takes the model** and ends every follower `cancelled`; with nothing running it answers `not_downloading`. A cancelled download leaves at most a `.part` file, which the next attempt replaces.
- **The first model:** below 16 GiB of total memory, `insufficient_memory` with the machine's total as `available_bytes`. Otherwise the smallest chat model for an Intel Mac or the `low` profile, else the manifest's default (Qwen3.5 4B today). An unknown profile is `bad_request`.
- `models.info` reports `downloading` and `not_installed`; streams and embeddings refuse a model that is not installed (`model_unavailable`).
- Downloads come only from the manifest's https sources. Tests inject a loopback source (`Gateway::with_source`) and serve the file from the test itself, held halfway for the attach and cancel cases.

## 2026-11-15 · Gateway: local models through isonomic-inference (SDK request R2)

- New crate `gateway`, pinned to isonomic-inference `4d01814` (`INFERENCE_REF`; CI passes `inference: true`). The broker hands `models` calls to it through `broker::Models`, after the token and capability checks.
- **Catalogue:** the model library's manifest. A model is installed when its file is in `<capsule>/models` at full size. `models.list` shows the installed models the caller may use; `models.info` reports the worker's state.
- **Choosing:** a named model, else the user's default. On a slow machine (an Intel Mac, or the low profile) the default chat model is the smallest installed one, following the inference benchmarks; otherwise the manifest's default if installed. `proto::auth::authorize_model` runs on the model chosen. Not installed answers `model_unavailable`; the wrong kind of model, `unsupported`.
- **Memory:** the engine's profile gate is checked before a worker starts, so `insufficient_memory` carries the gate's own numbers. The same gate is handed to the inference supervisor.
- **Workers:** one llama.cpp server per model, from the bundle, started on first use and unloaded when idle by the inference supervisor, with a per-launch API key. Without the bundled server, streams answer `model_unavailable`.
- **Streams:** text and reasoning chunks as they come, then exactly one chunk with `stop` (`end`, `max_tokens`, `error` with the reason, or `cancelled`). `models.cancel` belongs to the opening token and ends the stream with `stop: cancelled`; a closed connection drops the stream, which stops the server generating. `models.embed` runs on the embeddings model, in input order.
- **Agents:** `agents::GatewayModel` is the harness host's model client over the gateway (`local:default`, `local:smallest` or `local:<id>`). The engine binary runs agent sessions on it.
- **Verified** with the inference testkit's fake model server and a stand-in worker: default and slow-machine choice, filtered list, refusals, the gate's numbers, embeddings, owner-only cancel, and one Chat turn end to end through the broker, the agents host and the gateway, with the reply's text in the session's log.
- Not yet: tools on local models (the provider takes none: `unsupported`), usage numbers (the provider reports none), continuing a reply (`prefill` off), the download methods (isonomic-sdk #26, in review). The llama.cpp server is still a deferred sidecar in `sidecars.json`, so a packaged app does not carry it yet. **Done since:** usage counts streamed chunks, continue works (`prefill` on), the download methods (isonomic-sdk #26) and the bundled server (`b12387`). Tools on local models remain off: plain chat offers none.
