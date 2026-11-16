# Services: status

Newest first.

## 2026-11-15 · `appdb`: an app's schemas are made when it is granted

- `apps.grant` now sets the app's role, schemas and read access up right away, in the background after the reply, so the reply never waits on the database and a grant still succeeds while it starts. The setup before an app's first statement stays as the backstop.
- Checked with Chat's own manifest (`agents`, `sql`, schema `chat`): hermetically, the grant prepares it; on a real PostgreSQL 16, the `chat` schema exists owned by Chat's role, and Chat's `create table if not exists pins (… timestamptz …)`, insert and read work there.

## 2026-11-15 · `appdb`: dropped read grants are revoked; every write wakes live queries

- Re-granting an app whose manifest no longer lists a schema takes its read access back: usage, select on the tables there, and the default privilege for tables made later. Found from the role's own grants in the schema ACLs.
- Live queries wake on every committed transaction that wrote, whichever method ran it (a write sent through `sql.query` included) and whichever engine part shares the database. The database decides: a transaction id is assigned only on a write. Reads wake nothing.
- Both checked on the real server (`appdb`'s `real_database` test). The two deviations listed under R5 are closed.

## 2026-11-15 (later) · Quoted paths (isonomic-capsule PR #22)

- **Bug:** with the default Capsule directory (under "Application Support") the cache refused its config: the space split its socket path into two arguments. Every earlier check had set a Capsule directory without a space. The cache's socket and directory are now quoted, and so are the vector database's storage paths. Paths containing a double quote or a backslash still aren't escaped.


## 2026-11-15 · The cache; the vector database pinned; object storage deferred (isonomic-capsule PRs #14, #16, #17, #19)

**Built**

- The cache and queue service (#16), tier 1, started in parallel with PostgreSQL. Built from the pinned source (#14). A Unix socket in a user-only directory, no TCP port. Persistence off (no snapshots, no append log) until a use needs it. Memory capped by the profile: on Low the least recently used keys go first; elsewhere a full cache refuses writes rather than dropping queued work. Readiness is `PING` over the socket.
- PostgreSQL's buffers come from the profile (#17).
- The vector database (#19) is defined for the lazy tier: loopback on an engine-chosen port, a 256-bit key generated once into the Capsule (0600, never logged), its RPC listener and telemetry off, payloads on disk on Low, readiness `GET /readyz`. Pinned (1.20.1) for both Macs and Linux, but deferred: 75 MB (arm64) and 84 MB (x86_64) unpacked, so it ships with its first user in wave 3 (Omar, 15 Nov). The engine doesn't register it yet.

**Measured** (Intel Mac)

- Cache: warm start healthy in 0.28 s; force-killed, back in 1.08 s.
- The vector config against the real program: telemetry and the RPC listener reported off, listening on 127.0.0.1 only, 401 without the key and 200 with it, `/readyz` 200, clean stop on the termination signal.

**Deviations**

- Low's "memory-mapped" vector storage is only the payload setting so far. Vector storage is set per collection, so it lands with the first collection in wave 3.

**Deferred**

- Object storage: Milestone 4 (Omar, 15 Nov). Its program is 134 to 146 MB per architecture and stripping doesn't shrink it (capsule #10).
- Local models: the model gateway.
- From the entry below, now done: bundled binaries, profile-driven buffers and a run on macOS. Still open: one database per app (Milestone 4). Each app already has its own role. A still-running orphan is stopped since #23 (`docs/status/engine-supervisor.md`).

## 2026-11-14 · PostgreSQL supervised end to end; built locally, not yet on GitHub

**Built** (`supervisor::postgres`)

- First-time setup: `initdb` runs once, into a staging directory that is renamed into place only on success. A failed or interrupted setup leaves nothing that looks like a cluster. The bootstrap role is `isonomic`, encoding UTF-8, locale C.
- Exposure: a Unix socket in a user-only directory under the Capsule run directory. `listen_addresses` is empty, so there is no network listener at all. Local connections use trust auth; host connections are rejected.
- Readiness: the wire-protocol probe (startup handshake, then `SELECT 1`) over the socket.
- Unclean shutdown: detected from the leftover lock file. The start deadline is then 300 s, against 90 s after a clean stop.
- Stop: the fast-shutdown signal, then the usual grace and forced stop.

**Verified**

- Hermetic tests with a stand-in `initdb` and a fake server on a Unix socket.
- A manual run against a real PostgreSQL 16 on Linux, with misleading database variables set on the engine (`PGPORT`, `PGHOST`, `PGDATA`):
  - first-time setup created the cluster; the second run reused it
  - cold start 5.0 s (the probe cadence, see the supervisor status; the server itself is ready in about 0.1 s)
  - force-kill of the server: restarted on its own, healthy again after 6.0 s, restart counted in the registry
  - ordered stop: clean shutdown, lock file removed, no process left behind
- The command is `cargo run -p supervisor --example pg_smoke -- <postgres bin dir> <scratch dir>`.

**Deferred**

- Bundled binaries: the fetch task and the pinned manifest are not ported yet, so the engine has no PostgreSQL of its own to run.
- Profile-driven settings such as buffer sizes: Milestone 1.
- One database and one role per app: Milestone 4.
- An orphaned server from a crashed engine: the lock file is detected and recovery time is allowed, but a still-running orphan is not hunted down. **Done since:** #23.
- A run on macOS.

**Not started**

- Every other service. Out of scope for Milestone 0.
