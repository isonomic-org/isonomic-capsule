# Agents: status

Newest first. The host for agent sessions (`crates/agents`), over the harness host.

## 2026-11-16 · Agents: after a stop, the next reply waits until the model server has ended the stopped one

- **#79 did not hold on the Mac.** A continue straight after a stop still went to slot 1 and read 268 prompt tokens (2.97 s), so the cause was confirmed. No timed wait ran: the continue came later than #79's window, yet slot 0 was still busy.
- **What the engine does now:** a reply sent within 3 s of a stop first asks the model server's own slot list (`GET /slots`, with the launch's key) until no slot is working. It asks every 20 ms, for at most 1 s (`gateway::slots`). The server then gives it the slot whose cache matches it best, which is the conversation's. The engine logs `reply: the model server's slots were free after N ms`.
  - If a slot is still working at 1 s, the reply goes anyway, and the log says so.
  - If the server gives no slot list, the engine waits about one token gap instead (#79's rule), and the log says why.
- **The certain fix needs the inference repo.** That fix is to name the slot in the request (the server's `id_slot`), so a continue simply queues behind the stopped task on its own slot. But the provider builds the request body from `ChatRequest`, which is frozen and has no such field. It is a request to isonomic-inference, through the orchestrator.
- **Checked:**
  - against a local stand-in for the slot list: the reply waits until the stopped task has ended, it waits no longer than the limit, and a refused or missing slot list is said so, with the launch's key checked;
  - the stop test still passes, on the fallback, since the fake model server has no slot list.
- **The Mac check:** `turn_stages`'s stop-then-continue case now fails unless the continue lands on prompt 1's slot and evaluates fewer than 64 prompt tokens. It prints `continue: slot N (T prompt tokens); the conversation's slot is M`.

## 2026-11-16 · Agents: a continue after a stop lands on the conversation's slot

- **What the Mac measured:** a continue straight after a stop reached first text in about 5 s in three of five runs, and in 0.2 s in the other two.
- **The likely cause:** the chat worker has two slots. A stopped reply's request was dropped only when its next token came, and the model server frees the slot only when it next tries to send that reply a token. A continue sent before then found the conversation's slot busy, went to the other slot, which holds no cache of the conversation, and read the whole prompt again.
- **What the engine does now:**
  - **The stop drops the reply's request at once.** `sessions.cancel` raises `Gateway::reply_stopped`. A reply being written wakes on it, checks its own stop flag, and lets go.
  - **The next reply waits for the slot** (`agents::settle`). The engine remembers when a reply was stopped and how far apart its last tokens came. The next reply, a continue or a new prompt, waits until about one token gap plus 50 ms has passed since the stop, at most 600 ms, or 450 ms when fewer than two tokens came. It then finds the conversation's slot free.
  - **The wait is logged** as `reply: waited N ms for the stopped reply's slot`.
- **Checked:**
  - against the fake model server: a reply being written ends at once when it is stopped, the next reply waits the settle time and the log says so, and a reply long after a stop does not wait;
  - the settle times in a unit test.
- **The Mac check:** `turn_stages` gains a third case: a chat whose second prompt is stopped at its first text and continued at once. It prints the engine's `reply:` lines, the model server's slot and prompt-token lines for the continue, and "continue after a stop: first text".

## 2026-11-16 · Agents: the title rules now hold on the standard profile

- **#77 did not hold on the Mac.** A second prompt sent at once still waited 6.4 s: the title's model request started within about 0.2 s of the turn ending, and never yielded.
- **Why:** #77 told titles apart by the model `local:smallest`, which the preset names as its title model. But the harness uses that model only on the low profile. On the standard and high profiles, which Omar's Mac is, the title names the reply's own model. So no title matched, and none waited or yielded.
- **The fix:** a title is told apart by the harness's own title instruction, its system prompt (`harness_log::history::TITLE_PROMPT`), whatever model it names.
- **The engine says what each title did,** in its log:
  - `title: waiting 2000 ms for quiet`;
  - `title: sent`;
  - `title: yielded before it was sent`;
  - `title: yielded while it was being written`.
  `turn_stages` prints these lines in its timeline. The test no longer counts the first text twice; that had made the Mac run fail on its own bookkeeping.
- **Checked:**
  - the adapter tests now ask for titles on the reply's own model, as the standard profile does;
  - **new: through the whole engine on the standard profile,** a second prompt sent as soon as the first reply ends is answered in well under the quiet period, the chat gets its quick title, the model server never sees a title request, and the engine logs that the title yielded;
  - with the title rules off, the same test fails, because the chat gets the model's title, so it does test the fix.

## 2026-11-16 · Agents: a person's prompt never waits behind a title

- **What the Mac measured** (`turn_stages`, the 2B model). A second prompt sent as soon as the first reply ended reached its first text after 4.8 s, the same instant the title was written. One sent after the title took 1.8 s. The cache was reused either way (25 prompt tokens evaluated on turn 2), and the check took microseconds.
- **Why:** after turn 1 the harness writes the chat's title inside that turn, on the model's other slot, re-reading about 200 tokens. A new prompt waits for the turn, and so for the title.
- **What the engine does now** (`GatewayModel::with_title_quiet`, on in the engine):
  - a title request waits `TITLE_QUIET` (2 s) after its turn before it is sent;
  - if a person's turn begins meanwhile (`sessions.send` or `sessions.continue`, through `Gateway::foreground_began`), the title ends at once with `title.yielded`;
  - it also ends at once if the turn begins while the title is being written. Its model request is then dropped, so the model server stops it.
  The harness then gives the chat its quick title from the first prompt, and runs the person's turn. Replies never wait.
- **Title requests are told apart** by the harness's own title instruction, which is their system prompt (`harness_log::history::TITLE_PROMPT`). The model a title names depends on the profile: on the standard and high profiles it is the reply's own model.
- **What the engine cannot do alone:**
  - **cancel a prompt evaluation already running on the model server.** A batch in progress finishes first, which is why the quiet period matters most;
  - **try the model title again after the next turn.** The harness marks a chat titled before it tries, and gives the quick title when a title step fails;
  - **reuse the conversation slot's cache for the title.** The harness builds the title with its own system prompt, so it shares no prefix with the conversation.
  These are requests to the harness, filed through the orchestrator.
- **Checked:**
  - on tokio's paused clock: a title that meets a turn in its quiet period yields unsent, one that meets none is sent after the period, and a reply is sent at once;
  - against the fake model server: a title being written yields the moment a turn begins.
- **The Mac check:** `turn_stages` now fails if the second prompt sent at once reaches its first text later than one and a half times the after-title case plus a second. The engine's title rules are on in it.

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

## 2026-11-15 · Agents: the host's (session, id) keys (isonomic-harness #37)

- `SDK_REF` is `c0d2bf7`, and every harness crate is pinned to `f0612113`, the same URL and rev string as the SDK's, so cargo builds one copy.
- `approvals.decide`, `questions.answer` and `tools.result` pass the wire's `session` to the host, which now keys pending work by (session, id); the adapter's own checks of the pair are gone, except for a tool result: the host answers a late result for a call id never issued with `Ok` and an audit record, so the adapter still looks for the call in the session's log and answers `not_found` otherwise. `sessions.purge` passes the app (the caller's own, or the one the shell names). User-action tokens are minted and checked per (session, approval).
- Streams await `Subscription::next_update()`, durable events and live deltas alike, so the 40 ms drain is gone (isonomic-harness #32).
- `apps.grant` refuses the app id `shell`, which owns the shell's own sessions.
- Against `f061211`: the hermetic suite, the end-to-end turns, and conformance 28 of 28 pass.

## 2026-11-15 · Agents: stop and continue on local models

- `GatewayModel` now says it can continue a reply (`prefill`). A continue request ends with the sealed partial reply as the last assistant message, and the llama.cpp server continues a final assistant message (its assistant prefill, on by default in `b12387`).
- Verified end to end through the gateway with the fake model server: a reply is stopped after its first text, continued, and the server is asked with the partial text as the final assistant message.

## 2026-11-15 · Agents: the `agents` interface served by the harness host

- `SDK_REF` is `21ed50d` (isonomic-sdk #16 and #19: the agents interface and the `agents.` conformance checks). The SDK pins harness-core at the same URL and rev as this repo (`7a73ca6`). `welcome` carries `schema`, the session event schema the host writes.
- The broker hands every `agents` call, after the token and capability checks, to `broker::Agents`; `agents::EngineAgents` implements it over the harness host. Each of the 22 methods maps one to one onto a host call. `sessions.subscribe` replays the durable log after `from_seq`, then live events, as `session_event` and `live_delta` events; only the opening token ends it.
- The host is built from the engine's parts (`agents::start`): the SQLite session log, the token resolver over `broker::Apps`, Tokio for background work, the wall clock, and the system's locale (zone from `/etc/localtime`, offset from the C library, language from `LANG`). Limits and the policy ceiling are the harness defaults. `recover()` seals turns a previous engine left open before the broker serves.
- (session, id) is checked in the adapter for `approvals.decide`, `questions.answer` and `tools.result`, since the host takes the id alone (isonomic-harness #30). The shell's purge names an app and runs as that app.
- Conformance: **28 of 28**, the six `agents.` checks included, against the real agents host with a scripted model. End-to-end tests drive a Chat turn over the socket: the reply streams, the log replays from the store, another app's session answers `not_found`, purge is per app, stop is safe to repeat.
- Deviations:
  - Live deltas do not wake a subscriber in the host (isonomic-harness #32), so a stream also drains them every 40 ms. **Done since:** streams await `next_update` (see the entry above).
  - The engine binary runs the host with no model until the gateway lands: a turn ends with a readable `model_unavailable` error in the log. **Done since:** the gateway (#28).
  - Client tools are refused and attachments are kept in memory; V1 Chat uses neither.
  - The engine's profiles have no `intel` tier yet, so Intel Macs run under `low`, `standard` or `high` by memory.

## 2026-11-15 · Agents: the token resolver and user-action tokens

- `EngineScopes`, the harness host's `ScopeResolver`: a capability token resolves through `broker::Apps`, exactly as the broker resolves it, to `Caller::app(id, FirstParty)`; an unknown token is `Unauthorized`. Every app the engine grants is first-party; Views and third-party code get their kinds when they arrive.
- User-action tokens: `EngineScopes::mint(caller, approval)` when the shell reports that a person acted. Random (256 bits), single use (spent by the first check, right or wrong), bound to that caller's scope and that approval, and good for 120 s on the engine's injected clock. Kept in memory only.
- `broker::Services.apps` is now shared (`Arc`), so the broker and the agents host resolve the same tokens. harness-host is pinned at the same URL and rev as harness-core.
- Not yet wired: the broker has no method that mints a user-action token; that arrives with the `agents` interface (isonomic-sdk #16). **Done since:** `approvals.user_action` (#54).

## 2026-11-15 · Agents: the session log on SQLite (preparation)

- New crate `agents`, planned in `docs/plans/agents.md`: adapters over the harness host's traits, and nothing of the loop or policy.
- Built: harness-log's `SessionStore` over two tables in the registry database. One row per event (session, seq, schema version, payload), appended in one transaction that refuses a gap, an overwrite or another session's event. One registry row per session; deletes only through `purge`. Tested call for call against the harness's `MemoryStore`, and across a reopen.
- harness-core and harness-log come from isonomic-harness at the URL and rev isonomic-sdk #16 pins (`7a73ca6`), so cargo builds one copy of harness-core. CI passes `harness: true`; `HARNESS_DEPLOY_KEY` is visible to this repo.
- Not started, waiting on isonomic-sdk #16: the `agents` methods, `welcome.schema`, and moving `SDK_REF`. Waiting on the model gateway: `ModelClient`.
