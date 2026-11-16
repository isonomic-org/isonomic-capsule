# The Capsule's data across builds

A Capsule is the folder where the engine keeps everything a person has: their apps' grants, their sessions, their models. A Capsule made by one build must open in every later build; 0.9.0 to 1.0.0 is the first such step. This page says what in it is versioned, and what a change to it requires.

On macOS it is `~/Library/Application Support/Isonomic/Capsule` (`ISONOMIC_CAPSULE_DIR` overrides it); on Linux, `~/.local/share/isonomic/capsule`.

## The marker

`capsule.json` at the root names the data format. The engine reads it before it opens anything else (`capsule_core::format::open`):

| The marker says | The engine |
|---|---|
| nothing (no file) | Writes format 1. A Capsule from a build before markers is format 1 by definition; `created_by` is then `unknown`. |
| this build's format | Opens it, and records itself as `last_opened_by`. |
| an older format | Runs that format's migrations in order, then moves the marker on. There are none yet: 1 is the first format. |
| a newer format | Refuses, in plain words, and changes nothing, so the newer build can still open it. |

```json
{
  "format": 1,
  "capsule_id": "3f9c0a7e1b2d4c5f8e6a7b9c0d1e2f3a",
  "created_by": "0.9.0",
  "last_opened_by": "1.0.0"
}
```

`capsule_id` is 32 random hex characters, made once and then kept. It labels what the Capsule keeps outside its folder, the keychain items of its Isonomic ID, so a moved Capsule keeps them and two Capsules never share them. It was added without a format change: a marker without it gets one on the next open, and older builds ignore it.

`"scratch": true` marks a throwaway Capsule, such as the window's self-check. Whoever makes the folder writes it, before the engine first opens it, as `{"format": 1, "scratch": true}`. The engine never sets it and keeps it on every open. Only a scratch Capsule may keep its ID's keys in memory (`ISONOMIC_KEY_STORE=memory`), so a real Capsule never ends up with a key log whose keys are gone. On a real Capsule the variable is refused and logged.

## What format 1 is

| Path | What | Versioned by |
|---|---|---|
| `capsule.json` | The marker. | itself |
| `registry.db` | SQLite: `grants` and `tokens` (each granted app and its token's digest), `sessions` and `session_events` (agent sessions and their logs), `blobs` (apps' stored files), `services` (observed service state). | The Capsule format. Tables are made with `CREATE TABLE IF NOT EXISTS`, so a later build that changes a table's shape must bump the format and migrate. |
| `session_events.schema` | Each logged event carries the harness's event schema revision (1 today). | The harness. Every later harness must read every revision earlier builds wrote; the fixture test below holds the engine to it. |
| `secrets/token.key` | 32 random bytes, made on first start, user-only. Apps' tokens are derived from it, and only their digests are stored. | Never changes. Replacing it would revoke every app's token. |
| `models/` | One file per model, named by the model library manifest's `file_name`. A model is installed when its file is there at full size. `<name>.part` is a download not yet finished; the next attempt continues it. | The manifest. A later manifest that keeps a file name reuses the file; one that renames it downloads the model again. |
| `blobs/` | One file per blob, named by its handle; its row is in `registry.db`. | The Capsule format. |
| `identity/person.log` | The person's Isonomic ID: its key log, one entry per line, each the signed envelope's bytes in hexadecimal exactly as the identity library made them. Written whole and renamed into place. | The identity library's frozen version 1 format (isonomic-id `vectors/v1`). New in 0.9 without a format change: a Capsule without it gets an ID the first time the window asks. |
| `identity/logs/` | Every key log the engine has accepted (the person's own, and later each app's and agent's), one file per identity named by its ID's digest, in the same line format. Verified again at every start. | The identity library's version 1 format. |
| `identity/forks/` | A log that parted ways with the accepted one, kept whole for the person to decide, with where it parts and whether a recovery key signed it in its name. Never switched to on its own. | The same. |
| `identity/apps/<app>.id` | Each approved app's Isonomic ID. Its key log is in `identity/logs/`; its key in the key store, labelled `capsule/<capsule_id>/app/<app>/signing`. | The identity library's format. |
| `identity/grants/<app>.grants` | The grants the person gave the app, one per line, each the signed grant's bytes in hexadecimal. Replaced whole on a changed approval. | The identity library's frozen grant format. |
| `identity/revoked` | Every grant revoked, by its ID in hexadecimal, one per line. Only grows. | The same. |
| `registry.db`, table `id_resources` | What the person owns, for the check: each app's space, schemas and events (blobs and sessions from step 4). New in 0.9 without a format change: older builds ignore it, and grants are filled in for apps approved before it. | The Capsule format. |
| `identity/unreadable/` | A kept log that no longer verified at start, moved here rather than deleted. | The same. |
| Outside the folder: the login keychain | The ID's keys and the recovery seed, labelled `isonomic-id:capsule/<capsule_id>/person/…`: a P-256 signing key and a P-256 device rotation key (in the secure chip when the build may use it), and the recovery kit's 32-byte seed. | Never leave the keychain. The seed waits there until the recovery kit shows it to the person. |
| `services/postgres/` | The bundled database's data (PostgreSQL 18). | The database's major version, in its own `PG_VERSION`. The engine refuses a data directory of another major version, since one cannot read another's files. Moving to a new major version needs an upgrade step written for it, and a format bump. |
| `services/cache/` | The cache and queue service's data. | Its own. Disposable: losing it loses nothing a person made. |
| `reports/pending/` | Problem reports waiting to be shown, one JSON file each (`<unix millis>-<kind>.json`), written by the window and the engine. Each holds only scrubbed text; nothing is sent. The window prunes them (30 days, 20 at most). | The report format, 1 today, in each file's `format`. Older builds ignore the folder. |
| `unreadable/` | A `registry.db` that failed SQLite's check at start, moved here whole with its `-wal` and `-shm` as `registry-<UTC stamp>.db`, never deleted and never written. What still reads from it (chats, then stored files, then approvals) is copied into the new registry first; the engine's start message says how many chats came back and how many could not be read. | Not versioned; kept for recovery by hand. |
| `logs/` | `engine.log` and the services' logs. | Not versioned; read by people. |
| `run/` | Sockets (or `/tmp/isonomic-<uid>-<hash>/` when the Capsule's path is too deep for one). | Not versioned; made at every start. |
| `settings/` | The window's settings, such as `start-at-login-off`. | The window. |

## Changing the format

A change to any table's shape, to where something lives, or to what a file holds:

1. Bump `CAPSULE_FORMAT` in `crates/capsule-core/src/format.rs`.
2. Add the migration from the previous format, run by `format::open` before anything opens the data, and make it safe to run again after a crash part-way through.
3. Write a new fixture beside the old one (`crates/agents/tests/fixtures/capsule-format-<n>/`) with the build that writes the new format, and keep the old fixture's test passing: every earlier format must still open.

A new table, or a new column that older builds ignore and that has a default, is not a change of format; the older fixture's test says whether that holds.

## The test

`crates/agents/tests/older_capsule.rs` rebuilds the format 1 Capsule from `fixtures/capsule-format-1/` (the registry as SQL, the token key in hex, the token Chat was issued, its session) and opens it with this engine. The marker is added. Chat's old token still works without a new grant. Its session is listed with its title, its log reads back, and it takes a new message.
