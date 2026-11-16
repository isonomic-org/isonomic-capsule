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
- **The Isonomic ID's recovery key is on the same Mac** until the recovery kit exists (isonomic-id issue 14). Its seed waits in the login keychain for the kit to show it to the person, so for now it does not help if the Mac is lost. The ID never has to be made again for it.

New entries go at the top of this file, naming the area; older ones move to their area's file when this one nears 20 KB. The spec stays in `isonomic-desktop/docs/architecture/`.

## 2026-11-16 · Identity: the person's Isonomic ID at first need (R7, step 1)

- New crate `identity`, on the identity library at `ID_REF` `d39a507` (frozen at version 1, never changed from here). `identity.get` (shell only, isonomic-sdk #34, merged at `90eb83f`) answers the person's ID and whether that call made it; `identity_unavailable` carries the reason in plain words.
- **What is made, the first time the window asks:**
  - a P-256 signing key and a P-256 device rotation key, from the Mac key store: in the secure chip when the build may use it, else the login keychain, which an unsigned build always gets;
  - the recovery kit's 32-byte seed, kept in the login keychain. The recovery key is derived from it as the kit will derive it (the seed is the Ed25519 private key, RFC 8032) and committed in the first entry, since a key log never lets a recovery key be added later.
  - Then the key log is made, kind person. It is kept in `identity/person.log` in the Capsule, exactly as the library encoded it, and the ID is `did:iso:` and 64 hex characters, as the library gives it.
- **Every item is labelled for its Capsule** by a new `capsule_id` in `capsule.json`, made once and kept.
- **Order:** the seed is kept before anything commits to it, and the keys before the log names them. The log file is written whole and renamed into place. After a crash part-way, the next attempt uses what was kept.
- **Later calls read the Capsule's file and never touch the key store.** The key store is opened only to make an ID.
- **Off the Mac** (Linux, in CI and development), `identity.get` answers "Isonomic IDs are made with your computer's key store, and this computer has none Isonomic can use." Tests give the engine the identity library's in-memory store.
- **Checked:**
  - the ID is made once and read back by a later engine without the key store;
  - the log commits a device rotation key and the recovery key of the kept seed;
  - a crash before the log reuses the kept keys;
  - no key store and a refused keychain answer in plain words and write nothing;
  - a damaged record is refused and left as it is;
  - the derivation matches RFC 8032's own vector;
  - the older-Capsule fixture gets an ID on first open, then the same one;
  - conformance 31 of 31, `identity.stable` included;
  - the library's own `identities.rs` and `vectors.rs` are compiled here with this repo's lockfile and pass (`crates/identity/tests/library.rs`, reaching the frozen vectors through the `vectors` link at this repo's root);
  - the keychain code checked with clippy for both Mac targets.
- **No Ethereum binding at first run yet** (Omar decides). Adding it is one more entry where the ID is made, signed by the device rotation key.

## 2026-11-16 · Supervisor: nothing a service runs outlives the engine

- **A guardian beside every supervised service** (the database, the cache, and the lazy tier's services), approved by the orchestrator under rule 10.
  - It is a small shell reading a pipe that only the engine holds. The kernel closes the pipe when the engine dies, however it dies. The guardian then stops the service as a deliberate stop would: the service's own stop signal first (SIGINT for the database, its fast shutdown with a clean checkpoint), and a hard kill once the stop grace has passed.
  - A deliberate stop never wakes it. The monitor disarms a child's guardian once it reaps that child, and disarming kills the shell before the pipe closes. So a guardian never signals a process group whose number may be reused.
  - This is the same design as the model server's guardian in isonomic-inference.
- **Measured on a real PostgreSQL** with the first engine a child killed by SIGKILL: its database stopped 1.2 s later, cleanly (no `postmaster.pid` left, so no crash recovery on the next start), and the next engine started its own.
- **Reaping stays as the backstop** for a guardian that died too. Both reaping tests now kill the guardian first, found by the process group in its arguments, never by name.
- Tests: three for the guardian (stopped with the stop signal, killed after the grace when the signal is ignored, disarming leaves the service alone), and the three real-database cases in `crates/supervisor/tests/orphan_database.rs` (opt-in).

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
