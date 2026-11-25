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

## 2026-11-25 · Identity: the recovery kit on the wire

- **The four calls** the SDK added (`identity.kit_export`, `kit_restore`, `kit_saved`, `kit_status`) are answered by the broker, for the shell only. `broker::Identity` has them, refused by default; `IdentityService` does the work off the runtime (`identity::kit_calls`).
- **Errors:** every refusal is `recovery_refused`, with the identity crate's fixed sentences. A word is named by its position, never by what was typed, and the kit is never quoted. A key store or disk failure is `identity_unavailable`. Nothing in the broker logs a call or a reply.
- **`kit_saved` before any export** is refused, as the interface says.
- **Through the whole engine** (`agents/tests/recovery_kit.rs`): export on one Capsule, a fresh Capsule with its own key store, wrong words, a mistyped word and a planted file refused, restore to the same ID with a new kit, and a new app approval working with grants from the restored ID.
- **`SDK_REF`** points at the head of the SDK's recovery kit pull request (`1b7f2a5`) until it merges; it moves to the merge commit then. `UI_REF` follows that SDK commit's.

## 2026-11-17 · Identity: the recovery kit, engine side (no wire calls yet)

- **The kit document** (`identity::kit`, format `isonomic-recovery-kit`, version 1) is JSON holding:
  - the ID and when the kit was made;
  - `recovery`, a 16-character fingerprint of the recovery key;
  - the person's key log, each signed entry in hex;
  - `check`, the SHA-256 of the rest.
  A document that is not a kit, is newer, fails its check, or whose log does not verify is refused, in plain words.
- **The words are not in the file** (the orchestrator's decision). Export gives them beside the document, for the person to write down on paper; version 1 has no printing. Restore needs both, so a found file alone gives nobody the ID.
- **The words never reach** the engine's log, a problem report or an error message:
  - a refusal names a word by its position, never by what was typed;
  - the export and restore results' debug print hides the words.
- **The words** (`identity::words`) use the standard 24-word encoding of 32 bytes, with an 8-bit checksum. The standard 2048-word English list is bundled (`recovery_words.txt`) and pinned by its published SHA-256, and the standard's own vectors pass. A wrong count, an unknown word, or a mistyped or swapped word is refused before any key work; case and spaces are ignored.
- **Export** (`IdentityService::kit_export`) needs `person_asked`. It reads the seed from the key store, records `exported_at` in `identity/kit`, and logs `identity: recovery kit exported`, never the seed.
- **Restore** (`IdentityService::kit_restore`):
  - **checks:** the document verifies, the words' checksum holds, and the words derive the recovery key that the log's last entry commits to;
  - **new keys:** new device keys and a new recovery seed go in this Mac's key store, under the next generation (`identity/keys`);
  - **the entry:** one entry signed by the kit's recovery key retires every old signing key and commits the new keys. The ID does not change.
  - **the kit is spent:** a rotation key is used once, so the old kit cannot restore again from the new record. The restore returns a new kit, and `saved_at` is cleared until the person saves it.
  - **what the person is told:** the restore's `note` says the ID is back, that the kit used (with its date) is spent, to save the new kit and then destroy the old one, and that changes the old Mac made after the kit's date are set aside. The engine logs the same, without the words.
  - **one limit:** "spent" can be told only from the newer record. A spent kit used on yet another empty Mac still restores, making another fork, since that Mac knows nothing newer. That is why the person is told to destroy the old kit.
- **Where a restore is allowed:**
  - on a Capsule with no ID, always;
  - on a Capsule with an unused ID (no approved app, session or stored file), only with `replace_unused`. The old ID is moved to `identity/replaced-<stamp>/`, never deleted;
  - otherwise it is refused, and nothing changes.
- **An older kit,** exported before a later device rotation, still restores: device rotations cannot change the recovery commitment. Its entry then parts ways with the lost Mac's longer log. The library never resolves a fork: on the new Mac the restored log stays in force, and the longer one, if it turns up, is kept in `identity/forks/` and never adopted.
- **The resource records** are now cached by owner, so a new ID reopens them.
- **Checked** (`crates/identity/tests/kit.rs`, in-memory key stores, one per Mac):
  - export, then a fresh Mac, then restore: the same ID; the old signing keys are no longer current, and a new one is;
  - a new app's approval passes the check, and the kit's saved flag works;
  - the spent kit is refused;
  - refused plainly: wrong or swapped words, another kit's words, a damaged document, not a kit, a Capsule with apps, an unused ID without `replace_unused`, and export with no ID;
  - with `replace_unused`, an unused ID is set aside;
  - an older kit restores after a device rotation, and the lost Mac's log becomes a fork at entry 1;
  - **planted words:** after export, restore and refusals, no two words of either kit in a row appear in the document, the log ring, a refusal or a debug print, and a mistyped word is never echoed.
- **Not yet:** the wire calls (`identity.kit_export`, `kit_restore`, `kit_saved`, `kit_status`) wait for the SDK request. The broker will map these refusals to the new `recovery_refused` code.

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
