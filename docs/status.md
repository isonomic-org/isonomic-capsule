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

## 2026-11-16 · Packaging: every bundled program's licence is in the notice, or the fetch fails

- On the Mac run of `fetch-sidecars`, the model server had no licence text in `NOTICES.txt`. Its install predated `licenseFiles`, and its stamp still matched, so it was never installed again.
- **An install without its licence text is no longer current.** When the manifest names licence files and the install has none, it is installed again. The model server's archive carries `LICENSE`.
- **`fetch-sidecars` now fails** when any bundled program installed for this build has no licence text. This includes one whose manifest entry names no licence files. Programs installed for no target are not in the build and are not named. A release build cannot go out without every licence.
- Checked: an install with its licence removed is named as missing, then installed again with it, after which nothing is missing; the Linux fetch passes.

## 2026-11-16 · Problem reports: the engine writes its own, in the window's format 1

- **New crate `reports`.** The engine writes reports into the Capsule's `reports/pending/`, named and shaped as the window's own (format 1, `<millis>-<kind>.json`), so the window lists, shows and prunes them with its own. Each is written to a `.part` file and renamed. At most 20 are kept. Nothing is sent; no wire change.
- **Three kinds the window does not write:**
  - `panic`: the engine's panic hook, with a crate-relative backtrace and the last 50 lines of the engine's own log (a ring kept beside standard error);
  - `service_failed`: a bundled service's breaker trips. It names the service, its bundled version, its restarts and how it last ended (`exit code 1`, `signal 9`);
  - `model_server_failed`: a model worker quits unexpectedly (its first crash) and when its breaker trips. It names the model, the server and its version. A deliberate stop or an idle unload is not a failure.
- **`component`** is `engine`, `engine/<service>` or `engine/llama-server`, with its version. The engine adds `service` or `model` beside the window's fields. `tab` is always null. `pins.chat` is empty, since the engine does not know it. The build number and the engine's commit come from the release build (`ISONOMIC_BUILD`, `ISONOMIC_ENGINE_COMMIT`); without them, `dev` and empty.
- **The services are watched from outside.** The engine follows each supervisor's published state; the supervisors and the inference worker are unchanged. The gateway tells the engine of each new model worker (`Gateway::with_on_worker`).
- **What a report never carries.** The scrubber is a copy of the window's, marked to be replaced by the SDK's shared crate when it has one. It turns home paths into `~`, removes quoted text over 40 characters, and cuts the message at 300. It adds one rule the window lacks: a run of 24 or more key characters holding 6 or more digits becomes `[key removed]`. That covers tokens, digests and the person's ID. Service logs get harder rules first:
  - the database keeps only `LOG`, `WARNING`, `ERROR`, `FATAL` and `PANIC` lines, as their severity and first sentence. `STATEMENT`, `DETAIL`, `CONTEXT`, `HINT`, `QUERY` and continuation lines are dropped;
  - the model server keeps only an allow-list of prefixes (loading, the server's start, its errors), so request and slot lines are dropped;
  - every service line loses all quoted text and is cut at 200 characters.
- **Not set up for a refused Capsule.** The hook and watches start only after the Capsule's marker is accepted, so a Capsule from a newer build is left with no `reports/` folder.
- **Checked:**
  - planted content (a prompt, a statement's value, a home path and a token), printed by stand-in services that then fail through the real supervisors, reaches no report, both for the database and for the model server on a manual clock;
  - the hook writes a report with the engine's log before the process ends;
  - names and the keep limit match the window's;
  - a refused Capsule gets no report.
- **Not yet:** lines the agents host and the gateway print to standard error directly are not in the ring; only the engine's own log is.

## 2026-11-16 · Packaging: every bundled file runs on macOS 13.3

- **Found on the Intel Mac:** the release refused the disk image because the database said it needs macOS 26.0 (both architectures) and the cache 15.0. The model server was already 13.3.
- **The cache** was built on the Mac with no deployment target, so it took that Mac's macOS. Its recipe now sets `MACOSX_DEPLOYMENT_TARGET=13.3` and `-mmacosx-version-min=13.3`.
- **The database** was the vendor's prebuilt 18.4.0, whose minimum follows its build machines. Read here from the archives:
  - 18.0.0 and 18.1.0 say 13.0 on Intel and 15.0 on Apple Silicon;
  - 18.3.0 says 15.0 on both;
  - 18.4.0 says 26.0 on both.

  None targets 13.3 on Apple Silicon, and its tree carries OpenSSL, libxml and readline from the build machine.
- **On the Mac, the database is now built by `fetch-sidecars` from the official 18.4 source** (hash checked against the project's published `.sha256`), for macOS 13.3:
  - only what the engine uses: no ICU, readline, OpenSSL or XML, so it links only the system's own libraries;
  - layout and settings as before (`--without-icu`, `initdb --locale=C --encoding=UTF8`, default page and WAL sizes), so existing Capsules open unchanged;
  - install names rewritten relative to each file (`@loader_path`), as the vendor's were, and each file signed ad hoc;
  - Apple Silicon cross-built from an Intel Mac (`--host=aarch64-apple-darwin`).

  The developer tools (clang, bison, flex, perl) are needed. The Linux tree stays the prebuilt download.
- **`minimumMacos: "13.3"` in `sidecars.json`.** `fetch-sidecars` reads every Mach-O file it installs, every slice, and refuses (and removes) an install that says it needs a newer macOS, naming each file, architecture and minimum. An install already in place that fails the check is built again rather than kept.
- **The manifest takes a build per target** (the Mac's built from source, Linux's a download), and a build may produce a directory tree.
- **Tested:** thin and universal files' minimums; a tree with one too-new library refused and removed; a tree built from source per Mac target; a stale install rebuilt; no check without a stated minimum or off the Mac. The recipe's configure options build here on Linux; the macOS steps are for the Mac.
- **Built once, then cached:** an install's stamp holds the download's hash and, for a build, the digest of its recipe. A release build compiles the database only when its source or its options change. A clean build takes 92 s here (4 cores, Linux); on the Intel Mac, expect about 3 to 5 minutes per architecture, once.
- **Existing Capsules open unchanged:** a data directory made by the prebuilt 18.4.0 opens under the 18.4 built from source. An app's schema and row written under the old one are read, and written to, under the new one (`crates/appdb/tests/database_build.rs`, opt-in, run here as a normal user with both builds).
- **Licences:** each sidecar names its licence file (`licenseFiles`); `fetch-sidecars` keeps the text and refuses an archive without it, then writes `sidecars/NOTICES.txt` for the bundle to ship. What each licence asks is in [docs/third-party-notices.md](third-party-notices.md).
- **Not changed: the product's minimum, 13.3.**

## Known limitations of version 1

- **Same-user trust on the engine's socket.** The socket is user-only (0600, in a user-only folder), and a caller with no token is the shell. So any program running as the same user can do what the window can: read every app's data and sessions, grant apps, stop the engine. Other users on the Mac cannot. Most desktop apps share this limit. Tokens for the shell itself are planned for Milestone 4.
- **A full disk is refused as `too_large`**, with a plain message giving the space needed and free. A code of its own waits until after version 1.
- **Attachments are kept in memory** by the agents host and would not survive a restart. Chat's attach button is off in version 1.

New entries go at the top of this file, naming the area; older ones move to their area's file when this one nears 20 KB. The spec stays in `isonomic-desktop/docs/architecture/`.

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
