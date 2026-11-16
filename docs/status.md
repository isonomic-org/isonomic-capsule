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

## 2026-11-16 · Problem reports: on the SDK's shared crate

- **`SDK_REF` moves to isonomic-sdk `fedba7d`**: the shared crate `isonomic-reports` (landed at `f1b7376`) with its follow-up (UI at `b4c9670`). It is the window's crate too. Quoted text of any length is now removed from every scrubbed field, and a person's ID becomes `[id removed]` at any length.
- **The engine now uses it** for the format types, the scrubber (the key rule included), backtraces and the generic log rules. The engine's own copies are deleted. `System.macos` is now `os_version`; format 1 still writes it as `macos`.
- **What stays here:** which rules each service follows (`filter::Source::of`), the model server's prefix list (passed to `allow_listed`), the store, the reporter, the log ring, the panic hook and the watchers.
- **Checked:**
  - the planted-content tests pass on the shared rules;
  - a refused Capsule gets no report;
  - conformance passes 31 of 31.
- **Needs the identity stack first.** An `SDK_REF` past `90eb83f` brings in the `identity.get` conformance check, which only the identity stack serves, so this follows step 4.

## 2026-11-16 · Problem reports: the agents host's and the gateway's lines are in the log ring

- The engine's log ring moved to `capsule_core::log`, so every engine crate writes into the one ring a panic report carries. `reports::log` re-exports it.
- The two crates that printed to standard error directly now write through it: the agents host's refused calls (`info: <method> from <app> refused: <code>: …`) and the gateway's chat-template warning. Nothing in the engine prints around it now. The harness and the inference crates print nothing.
- Checked: a refused agents call is in the ring.

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
- Lines the agents host and the gateway printed directly were added to the ring in the entry above.

## Known limitations of version 1

- **Same-user trust on the engine's socket.** The socket is user-only (0600, in a user-only folder), and a caller with no token is the shell. So any program running as the same user can do what the window can: read every app's data and sessions, grant apps, stop the engine. Other users on the Mac cannot. Most desktop apps share this limit. Tokens for the shell itself are planned for Milestone 4.
- **A full disk is refused as `too_large`**, with a plain message giving the space needed and free. A code of its own waits until after version 1.
- **Attachments are kept in memory** by the agents host and would not survive a restart. Chat's attach button is off in version 1.
- **Until the person has an Isonomic ID, apps are held by their manifests alone.** The person's grants are checked on every data call once the ID exists (first run makes it before any app opens). Off the Mac, where no ID can be made, the manifests are all there is.
- **The Isonomic ID's recovery key is on the same Mac** until the recovery kit exists (isonomic-id issue 14). Its seed waits in the login keychain for the kit to show it to the person, so for now it does not help if the Mac is lost. The ID never has to be made again for it.

New entries go at the top of this file, naming the area; older ones move to their area's file when this one nears 20 KB. The spec stays in `isonomic-desktop/docs/architecture/`.

## 2026-11-16 · Identity: every app data call goes through the check (R7, step 4)

- **The broker asks the identity service about every call an app makes**, after the manifest's own checks (`proto::auth`) and before the call runs. A call becomes an action on a resource (`identity::check`):
  - `blobs.get` and `blobs.info` read the app's space; the other blob calls write it;
  - `sql.query` and `sql.subscribe` read the schema they run in; `sql.execute` and `sql.batch` write it;
  - `events.subscribe` reads the app's events; `events.publish` writes them;
  - agents calls that only look read the app's space; the rest write it;
  - unsubscribing, models and the shell's own calls are not checked.
- **Blobs and sessions are checked against their space.** Every version 1 grant covers whole spaces, so the answer is the same as for each one; they get records of their own once a grant can name one.
- **The decision** is the identity library's own check through its `ChainCache`. Each grant the app holds is tried as its own chain, and `VersionedWorld` moves with the directory and the revocations.
  - A denial answers `capability_denied` with the reason. An "ask first" answer is refused for now, until step 5.
- **The warm path reads nothing from disk.** An app's ID and grants, the revocations and resource records are held in memory, and dropped on any change.
- **Measured here** (release, 4 cores), per call:

| Call | First call | Warm |
|---|---|---|
| `blobs.get` | 268 µs | 1.35 µs |
| `sql.query` | 115 µs | 1.59 µs |
| `sessions.subscribe` | 4.3 µs | 1.29 µs |
| `sessions.send` | 4.1 µs | 1.28 µs |

  The first call per app verifies its chain once.
- **A Capsule from before step 3 keeps working.** An app with no grants yet gets them on its first call (off the runtime, since the key store may be asked), and the call is checked after. Shown at the service and by the older-Capsule fixture, whose Chat keeps working through the ID, the fill-in and the checks.
- **A revoked grant is refused after a restart**, with the reason.
- **Without the person's ID** (before first run's ID step, or off the Mac) the manifest's checks alone hold, as before R7. This is listed in the known limitations.
- Conformance 31 of 31 with the check on.

## 2026-11-16 · Identity: apps get an ID and grants from the person (R7, step 3)

- The design the orchestrator approved:
  - the person owns every resource;
  - each app has a space;
  - schemas and an app's events are resources, rows and topics are not;
  - app grants never expire, and end only by revocation.
- **On `apps.grant`, once the approval is recorded and before the reply,** the app gets:
  - **its own key log**, kind app, naming the person as its controller, with one Ed25519 key in the key store. The log is accepted into the directory, and `identity/apps/<app>.id` names it;
  - **its space**, its declared schemas and its events in the new `id_resources` table of `registry.db`;
  - **grants from the person**, each a one-link chain that cannot be passed on: read and write on its space, and read on each other app's schema its manifest lists. A schema listed before its owner is granted is recorded on its own, and claimed by the owner's space when it is.
- **A re-grant with the same capabilities keeps the grants.** A changed one revokes the old grants in `identity/revoked` first, then keeps the new ones in `identity/grants/<app>.grants`.
- **Tokens are unchanged:** the engine finds an app's grants by the app its token stands for.
- **Until the person has an ID, nothing is issued.** When the ID is made, every app approved before it gets its grants (`IdentityService::fill_in`). The engine also runs the fill-in in the background at start, for a Capsule whose apps were approved before grants existed. Off the Mac there are no IDs, so no grants; the manifest's checks still hold.
- `identity::world::decide` tries each grant an app holds as its own chain, through the library's `ChainCache`: the first that allows decides, then asking a person, then denial. Step 4 uses it on every data call.
- **Checked** with the library's own check function:
  - an app's ID names the person, stays the same across re-grants and restarts, and its log is kept;
  - the space grant allows read and write on the app's own schema and events, and nothing of another app's;
  - a listed schema can be read but not written;
  - a schema listed early is claimed by its owner;
  - the same re-grant keeps the grants, and a changed one revokes them, so an old grant no longer works and the new ones do;
  - the older-Capsule fixture's Chat gets its grants when the ID is made, a newly approved app gets them on approval, and Chat keeps working;
  - conformance 31 of 31.

## 2026-11-16 · Identity: the directory of accepted logs, kept (R7, step 2)

- `identity::directory::Logs` keeps every key log the engine accepts across restarts: the person's own now, and each app's and agent's from step 3 on.
- **It is the identity library's `Directory`, as it decides:**
  - a new log or a longer continuation is kept;
  - an older copy changes nothing;
  - a log that parts ways is refused as a fork.
- **Kept:** one file per identity in `identity/logs/`, named by the ID's digest, in the same line format as the person's log. The file is written before the directory in memory changes (persist, then publish), and every file is verified again at start. Logs are verified with the Ethereum binding checker, so a log with a binding verifies.
- **A fork is never resolved here.** The competing log is kept whole in `identity/forks/`, with where it parts and whether a recovery key signed it, and the known log stays in force. Showing it to the person needs a wire call that does not exist yet: the smallest would be a shell-only `identity.forks {}` listing `{ id, at, by_recovery }`, for after version 1's first-run step.
- **A kept log that no longer verifies** at start is moved to `identity/unreadable/`, never deleted. The engine's log says so.
- `Logs::version` goes up on every change, for step 4's `VersionedWorld`. `Logs` answers `KeyResolver` for grant checks.
- **Checked:**
  - a log is kept and comes back after a restart;
  - a longer one replaces it while an older copy changes nothing, and the version moves only on a change;
  - a fork signed by the recovery kit is refused, kept whole and listed, and the known branch stays after a restart;
  - a damaged kept log is set aside and said, and the others load;
  - a log with an Ethereum binding verifies;
  - the person's own log is in the directory.

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
