# Engine status

Newest first. This file is the engine's status from now on. The history of each area, moved here from `isonomic-desktop/docs/architecture/<area>/status.md` at that repo's commit `eb30c136`, is in `docs/status/`:

- [Broker](status/engine-broker.md): the socket server and the wire interfaces (tokens, blobs, events, sql).
- [Agents](status/agents.md): agent sessions over the harness host.
- [Gateway](status/gateway.md): local models, the catalogue, downloads and the chat workers.
- [Registry](status/engine-registry.md): observed state, grants.
- [Engine core and supervisor](status/engine-supervisor.md): profiles, the lazy tier, the RAM gate.
- [Services](status/engine-services.md): the database, the cache, the vector database.
- [Packaging](status/packaging.md): bundled services and the app bundle.
- [Identity](status/identity.md): the person's Isonomic ID, apps' IDs and grants, the check on every call.
- [Problem reports](status/reports.md): the engine's own reports, the log ring, the shared crate.
- [Later modules](status/engine-later.md): reserved crates.

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

## 2026-11-16 · Identity: approving the same app again signs nothing

- **The fault the Mac found** (build 0.9.0-143, the self-check's link run). On a scratch Capsule's second start, the window approved Chat again, as every launch does, and the engine answered `identity_unavailable`: "Your computer's key store didn't let Isonomic make your ID (no such key)". So the window had no token for Chat, and Chat never mounted.
- **Why:** the engine's grant step opened the person's signing key before it checked whether the app already held the same grants. A scratch Capsule's in-memory keys are gone after its first run, so that open failed although nothing needed signing.
- **The fix:** the grant step reads the person's ID from the Capsule, and opens the signing key only when grants must be signed: a new app, or changed capabilities. Approving the same app again, on any Capsule, no longer touches the key store at all. On a real Mac with an unsigned build, this also spares a keychain access at every launch.
- **When signing is needed and the key is not there,** the engine now says so plainly instead of the misleading "didn't let Isonomic make your ID": "Your computer's key store doesn't have the key that signs for your Isonomic ID (…), so Isonomic can't approve a new app with it. Nothing was changed." The wire code is unchanged.
- **Checked** (the engine on an empty in-memory store, as a restart leaves it):
  - the same ID is read from the Capsule;
  - the same approval succeeds;
  - Chat's data calls pass the check;
  - a new app's approval is refused with the plain message.

## 2026-11-16 · Registry: a damaged registry gives back what it still can

- **Before the engine starts on a new registry,** what can still be read from the set-aside copy is copied into it (`registry::salvage`). Order:
  - the chats (`sessions` and their `session_events`);
  - the list of stored files (`blobs`);
  - the approvals (`grants` and `tokens`) and the ID's records;
  - indexes, which are made anew last, so a damaged index costs nothing.
  `services` is not copied: it is rebuilt at start.
- **The set-aside copy is never opened.** The work runs on a throwaway duplicate with its `-wal`, so the latest writes are read too, and the duplicate is removed afterwards.
- **Row by row, by key,** because a damaged page then costs only its own rows:
  - tables with rowids are read one rowid at a time;
  - a chat's events are read one sequence number at a time;
  - a row that fails to read is counted as lost, and a row that is not there was deleted;
  - probes are bounded, so a table whose root is damaged cannot hold up the start.
- **The start message says what came back,** for example "297 chats recovered, 3 could not be read. Approved apps keep their approvals." If nothing in the file could be read, it says so, and that apps must be approved again. In every case it says that data and files are kept, and that what was not recovered stays in the set-aside copy.
- **Checked, on a registry built with the engine's own table definitions:**
  - a damaged header gives back nothing, and says so;
  - one damaged page of the chats table costs only that page's chats, while every approval, every stored file and every recovered chat's history comes back;
  - a damaged page inside one chat's history: that chat comes back with the events on both sides of the page and is reported as missing part of its history, and every other chat is whole;
  - a damaged index costs nothing;
  - in each case the set-aside copy is unchanged byte for byte, no working copy is left, and the new registry passes the check.

## 2026-11-16 · Engine core and broker: a damaged registry is set aside; the socket under hostile input

- **A damaged `registry.db` no longer stops the engine.** Before anything opens it, the engine runs SQLite's `quick_check` (`registry::check`). The check opens the file read-only and immutable, so it writes nothing and leaves `-wal` and `-shm` as they were.
- **When the file fails the check,** it is moved whole, never deleted, into the Capsule's `unreadable/` folder, as `registry-<UTC stamp>.db` with its `-wal` and `-shm` beside it. The engine then starts with an empty registry and logs plainly what that means:
  - apps must be approved again;
  - their data in the database and their files are kept;
  - the chats from before stay in the set-aside copy and are not shown.
  Re-approval gives an app the same token, since tokens are derived from `token.key`.
- **The socket under hostile input** (`crates/agents/tests/wire_fuzz.rs`). The test engine is sent:
  - random bytes before any greeting;
  - lengths of 0, 1, just over the limit and `u32::MAX`;
  - a frame cut short and held open, and a connection that never greets;
  - JSON nested 100,000 deep;
  - 400 client requests from the SDK's frozen fixtures, mutated on four connections at once (flipped bytes, cut short, spliced junk, huge values).
  The engine never panicked, answered at least nine in ten of the mutated requests (every frame with an intact length is answered), and a well-behaved client kept getting answers throughout. It is seeded, so every run sends the same input.
- **Checked:**
  - a healthy or missing registry is left alone, and the check touches nothing;
  - a damaged one is moved aside byte for byte with its `-wal`;
  - a path with spaces, `#` and `%` is checked where it is;
  - stamps read as UTC;
  - the fuzz run above.

## 2026-11-16 · Engine core and reports: the engine's own log rotates; a run of identical panics is one report

- **`logs/engine.log` is written by the engine now**, through `capsule_core::log`. It is rotated past 1 MiB, keeping one previous file (`engine.log.1`), like the services' logs. Lines also go to the terminal when the engine runs in one.
  - Lines from before the Capsule is accepted still go to standard error, so a refused Capsule gets no log written into it.
  - Until now the login agent sent standard error straight into `engine.log` and nothing rotated it.
  - The window's plist should point `StandardErrorPath` at `logs/engine.stderr`, which then holds only what comes before the log opens and a panic's own output. I asked isonomic-desktop for this through the orchestrator.
- **A run of identical panics is one report.** A panic with the same message and component as a pending panic report from the last 10 minutes updates that report rather than writing another:
  - `repeats` counts the panics after the first;
  - `when.last_unix_secs` says when the latest was;
  - the report keeps its `id`.
  An engine that panics at every start is relaunched by the login agent each time, so this replaces up to 20 identical reports with one. Other kinds are never merged.
- **Checked:**
  - the log rotates, keeping one previous file, and reopening appends;
  - five panics two minutes apart make one report with `repeats` 4, while a different panic, or the same one after 10 minutes, is its own report;
  - a refused Capsule gets neither a report nor an `engine.log`.

## Known limitations of version 1

- **Same-user trust on the engine's socket.** The socket is user-only (0600, in a user-only folder), and a caller with no token is the shell. So any program running as the same user can do what the window can: read every app's data and sessions, grant apps, stop the engine. Other users on the Mac cannot. Most desktop apps share this limit. Tokens for the shell itself are planned for Milestone 4.
- **A full disk is refused as `too_large`**, with a plain message giving the space needed and free. A code of its own waits until after version 1.
- **Attachments are kept in memory** by the agents host and would not survive a restart. Chat's attach button is off in version 1.
- **Until the person has an Isonomic ID, apps are held by their manifests alone.** The person's grants are checked on every data call once the ID exists (first run makes it before any app opens). Off the Mac, where no ID can be made, the manifests are all there is.
- **The Isonomic ID's recovery key is on the same Mac** until the recovery kit exists (isonomic-id issue 14). Its seed waits in the login keychain for the kit to show it to the person, so for now it does not help if the Mac is lost. The ID never has to be made again for it.

New entries go at the top of this file, naming the area; older ones move to their area's file when this one nears 20 KB. The spec stays in `isonomic-desktop/docs/architecture/`.
