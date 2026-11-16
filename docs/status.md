# Engine status

Newest first. This file is the engine's status from now on. The history of each area, moved here from `isonomic-desktop/docs/architecture/<area>/status.md` at that repo's commit `eb30c136`, is in `docs/status/`:

- [Broker](status/engine-broker.md): the socket server and the wire interfaces (tokens, blobs, events, sql).
- [Agents](status/agents.md): agent sessions over the harness host.
- [Gateway](status/gateway.md): local models, the catalogue, downloads and the chat workers.
- [Registry](status/engine-registry.md): observed state, grants.
- [Engine core and supervisor](status/engine-supervisor.md): profiles, the lazy tier, the RAM gate.
- [Services](status/engine-services.md): the database, the cache, the vector database.
- [Packaging](status/packaging.md): bundled services and the app bundle.
- [Later modules](status/engine-later.md): reserved crates.

## Known limitations of version 1

- **Same-user trust on the engine's socket.** The socket is user-only (0600, in a user-only folder), and a caller with no token is the shell. So any program running as the same user can do what the window can: read every app's data and sessions, grant apps, stop the engine. Other users on the Mac cannot. Most desktop apps share this limit. Tokens for the shell itself are planned for Milestone 4.
- **A full disk is refused as `too_large`**, with a plain message giving the space needed and free. A code of its own waits until after version 1.
- **Attachments are kept in memory** by the agents host and would not survive a restart. Chat's attach button is off in version 1.

New entries go at the top of this file, naming the area; older ones move to their area's file when this one nears 20 KB. The spec stays in `isonomic-desktop/docs/architecture/`.

## 2026-11-16 · Gateway: a model stays loaded for its profile's idle time

- A local model's worker is unloaded after the profile's idle time (`Profile::model_idle_unload`): 5 min on low, 15 min on standard, never on high. Before this, every profile used the inference supervisor's default, 5 min. On the 16 GB Intel Mac (standard), a reader who answered after 5 minutes paid a cold load (about 5 s there) and a full re-read of the conversation. The test covers each profile and the default.
- The reader's pass over the docs (Worker reports): deferrals since done are marked so in their area files; the known limitations of version 1 are listed above.
- **A database left by an engine killed outright is stopped before the next one starts.** The Intel Mac showed it: after `kill -9` of the engine, the model server was gone (its guardian), but PostgreSQL and the cache kept running until the next start.
  - Reaping (#23) already stopped them, by the engine's pid files in the run directory.
  - If the run directory had been cleaned meanwhile, the next engine missed the old database. Its own server could not start on the locked data directory, and its readiness probe answered from the old server's socket. Checked here: the new engine "started" against a server it did not own.
  - Now the service's own pid file (PostgreSQL's `postmaster.pid`) is read when the engine's is gone.
  - Tested on a real PostgreSQL with a real orphan: the first engine is a child process killed with SIGKILL, in both cases (`crates/supervisor/tests/orphan_database.rs`, opt-in).

## 2026-11-16 · Engine core: a Capsule made by one build opens in the next

- **The marker.** `capsule.json` names the data format (1), who made the Capsule and who last opened it. The engine checks it before it opens anything else (`capsule_core::format::open`):
  - no marker: format 1 is written (every Capsule from before markers is format 1);
  - an older format: migrated (none yet);
  - a newer one: refused in plain words, with nothing changed.
- **The database.** The bundled database's data directory must be this build's major version (18, from its own `PG_VERSION`), else it is refused the same way.
- **What is versioned**, and what a change to it requires, is in [docs/capsule-format.md](capsule-format.md): the registry's tables, the harness's per-event schema revision, the token key (never changes), the models folder (by the manifest's file names) and the database's major version.
- **Tested** with a format 1 Capsule kept as text (`crates/agents/tests/fixtures/capsule-format-1/`) and opened by this engine (`older_capsule.rs`). Chat's old token works without a new grant, and its session is listed with its title. Its log reads back, and the session takes a new message.
- [docs/uninstall.md](uninstall.md) says in plain words what Isonomic puts where on a Mac and how to remove it, keeping the data or not.
- The engine's version moves from 0.0.1 to 0.9.0, the app's numbering for test builds, so the marker names the same version the app does.
- Pins: isonomic-sdk `439a069` (only its `UI_REF` moved), `UI_REF` `2e3116b`, the components that passed QA. The harness stays at `ae4f1a7`.

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

## 2026-11-16 · Agents: harness `ae4f1a7`, chat preset version 3

- Pins: isonomic-sdk `99630cc`, harness `ae4f1a7`.
  - Chat preset version 3 asks for shorter answers, with `max_output_tokens = 1024`.
  - No request is larger than the model's window: the context the gateway reports (8192 per slot) minus the reply's limit.
  - A message too long for it ends its turn `failed` with `context.too_large`.
  - The window is the picked model's, when a session names one. Conformance: 30 of 30.
- **Measured here** (4 cores, llama.cpp `b12387`). Through the engine, "Plan one day in Lisbon." in six fresh sessions (`real_model`'s `the_same_first_request_in_fresh_sessions`).
  - **Qwen3.5 2B:** 182 to 368 tokens. Warm runs took 15 to 26 s, with first text in 0.3 to 1.8 s; the first run took 43 s with the model's load. On the same server and prompt, preset version 2 wrote 902 and 1080 tokens in 92 and 81 s.
  - **Quality, 2B:** it refused none of the six. Every reply names real places (Belém Tower, the Jerónimos Monastery, São Jorge Castle, Alfama, Chiado). Every one also has mistakes: Porto's sights placed in Lisbon, a "Louvre Palace Museum", a train to Funchal.
  - **Qwen3.5 0.8B:** 110 to 227 tokens in 4 to 8 s warm. It refused none, but nearly every place is invented or in another city, as before.
- **On the Intel Mac** (release build 96, acceptance check): the second turn's first text went from 27.5 s to 2.62 s with the cache fix of #53.

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
