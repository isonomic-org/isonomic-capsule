# Gateway status

Newest first. The model gateway: local models through isonomic-inference, the catalogue, downloads and the chat workers. Moved here from `docs/status.md`.

## 2026-11-16 · Gateway: a model stays loaded for its profile's idle time

- A local model's worker is unloaded after the profile's idle time (`Profile::model_idle_unload`): 5 min on low, 15 min on standard, never on high. Before this, every profile used the inference supervisor's default, 5 min. On the 16 GB Intel Mac (standard), a reader who answered after 5 minutes paid a cold load (about 5 s there) and a full re-read of the conversation. The test covers each profile and the default.
- The reader's pass over the docs (Worker reports): deferrals since done are marked so in their area files; the known limitations of version 1 are listed above.
- **A database left by an engine killed outright is stopped before the next one starts.** The Intel Mac showed it: after `kill -9` of the engine, the model server was gone (its guardian), but PostgreSQL and the cache kept running until the next start.
  - Reaping (#23) already stopped them, by the engine's pid files in the run directory.
  - If the run directory had been cleaned meanwhile, the next engine missed the old database. Its own server could not start on the locked data directory, and its readiness probe answered from the old server's socket. Checked here: the new engine "started" against a server it did not own.
  - Now the service's own pid file (PostgreSQL's `postmaster.pid`) is read when the engine's is gone.
  - Tested on a real PostgreSQL with a real orphan: the first engine is a child process killed with SIGKILL, in both cases (`crates/supervisor/tests/orphan_database.rs`, opt-in).

## 2026-11-16 · Gateway and agents: what a first-time user can hit

- **Downloads survive a quit, sleep or lost connection** (`gateway::fetch`, in place of the model library's downloader, which always started over and could wait forever on a dead connection).
  - The file arrives as `<name>.part`; the next attempt continues it with a range request, re-reading what is on disk into the checksum. A server that ignores the range gets a clean restart.
  - A transfer that sends nothing for 60 s fails and keeps its part. Progress starts from what is already on disk.
  - Only a whole file whose SHA-256 matches is renamed into place, so a half file never counts as installed. A part that fails the checksum is thrown away; a whole part left before its rename installs without a request.
  - An explicit cancel also keeps the part, for the next attempt.
- **Disk space is checked before a download starts**: the bytes still missing plus 1 GiB to spare. Without room the call is refused (`too_large`, the nearest code the wire has) in plain words: "There is not enough free disk space to download Qwen3.5 2B: it needs 2.4 GB and 0.9 GB is free. Free up some space, then try again." A volume that cannot be read is not refused.
- **A crash mid-turn leaves the session usable.** Checked by dropping an engine's runtime mid-reply, with nothing shut down, and starting another on the same Capsule:
  - the socket binds again;
  - the interrupted turn is sealed `interrupted`;
  - the session takes the next message and the turn ends `done`;
  - no worker process is left behind.
- **On the Intel Mac** (`later_turns`, capsule `fad9379`, the 2B model, idle): turn 1 first text 11.73 s; turns 2 and 3 first text 0.42 s and 0.39 s (each about 8 s in all).

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
