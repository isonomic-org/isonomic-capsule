# Engine core and supervisor: status

Newest first. Every deviation and deferral is recorded here.

## 2026-11-16 · Supervisor: nothing a service runs outlives the engine

- **A guardian beside every supervised service** (the database, the cache, and the lazy tier's services), approved by the orchestrator under rule 10.
  - It is a small shell reading a pipe that only the engine holds. The kernel closes the pipe when the engine dies, however it dies. The guardian then stops the service as a deliberate stop would: the service's own stop signal first (SIGINT for the database, its fast shutdown with a clean checkpoint), and a hard kill once the stop grace has passed.
  - A deliberate stop never wakes it. The monitor disarms a child's guardian once it reaps that child, and disarming kills the shell before the pipe closes. So a guardian never signals a process group whose number may be reused.
  - This is the same design as the model server's guardian in isonomic-inference.
- **Measured on a real PostgreSQL** with the first engine a child killed by SIGKILL: its database stopped 1.2 s later, cleanly (no `postmaster.pid` left, so no crash recovery on the next start), and the next engine started its own.
- **Reaping stays as the backstop** for a guardian that died too. Both reaping tests now kill the guardian first, found by the process group in its arguments, never by name.
- Tests: three for the guardian (stopped with the stop signal, killed after the grace when the signal is ignored, disarming leaves the service alone), and the three real-database cases in `crates/supervisor/tests/orphan_database.rs` (opt-in).

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

## 2026-11-15 · Engine core: the memory gate reads macOS as macOS counts memory

- Found on the 16 GB Intel Mac in ordinary use: the gate refused Qwen3.5 2B and even the 0.5 GB model, twice in four runs. It read 1.0 to 1.6 GB available while the Mac had about 160 MB free but 5.5 GB of inactive pages, which macOS hands out on demand, and its pressure was normal. On top of that it added a fixed 2 GB headroom, more than the models themselves.
- **Reading (`capsule_core::memory`):** on macOS, available is now free plus inactive plus speculative pages from the kernel's own VM statistics (`host_statistics64`). Purgeable pages are not added, because they already sit in the active or inactive queues. The system's pressure level is read too (`kern.memorystatus_vm_pressure_level`). Other systems keep their own "available" figure.
- **Gate (ported; changed with the orchestrator's go-ahead):**
  - Headroom grows with the model: half its size, and never under 512 MiB.
  - Critical pressure refuses whatever the counts say; a warning alone does not.
  - The refusals say in plain words what to do: close some apps or browser tabs and try again, or choose a smaller model. The amounts are given in GB. The remote provider is no longer suggested (V1 is local only).
- **Tests:** the Mac's exact page counts now admit both models; the earlier figures still refuse the 2B model in plain words; the pressure cases; the headroom rule. The ported tests are kept and adapted. Cross-checked for both Mac targets with clippy.
- To see what the gate sees on a machine: `cargo run -q -p capsule-core --example memory_check`.

## 2026-11-15 (later) · A second engine, quoted paths, left-behind services (isonomic-capsule PRs #21 to #23)

- A second engine for the same Capsule exits 0 instead of failing (#21), so the login agent doesn't restart a duplicate in a loop.
- Services left running by a killed engine are stopped before the next engine starts its own (#23). `ServiceConfig::pid_file` (opt-in; ported behaviour unchanged): every spawn notes the pid, a deliberate stop removes it, and start stops whatever it names, if alive and running a program of the same name, with the service's stop signal and grace. PostgreSQL and the cache have pid files in the run directory. This closes the "still-running orphan" deferral below.
- Measured on the Intel Mac through the login agent: `kill -9` on the engine to both services healthy, 5.1 s.


## 2026-11-15 · Profiles applied, the lazy tier and the RAM gate (isonomic-capsule PRs #17, #19, #20)

**Built**

- Profiles drive configuration (#17): the engine picks one at start (detected from total memory, or `ISONOMIC_PROFILE`; an unreadable override is logged and a fresh probe used) and renders it into each service's config: database buffers and cache memory per the table. The log line says which profile and why.
- The lazy tier (#19): `Stack::ensure_started` starts a tier-2 service on first use. Concurrent callers share one start; a failed start reaches every caller that waited on it, and the next call tries again. A tier-2 service that hasn't started is left out of `system.status`, so it doesn't hold the light off green.
- The RAM gate (#20), carried over from the earlier build with its tests: the profile ceiling first (4 GiB Low, 10 GiB Standard, none on High), then free memory plus 2 GiB headroom. The refusal has the SDK's `memory` error fields and points to a smaller model or a remote one. A tier-2 service registered with a footprint is checked before every start; a refusal starts nothing. A gated service with no gate is refused, not started unchecked.

**Decision: one gate.** The inference workspace declares the same seam and ships only a fixed-budget gate for tests, leaving the hardware-aware gate to the engine. So the engine's gate is the only real one; the model gateway adapts it to the inference trait when it takes that dependency.

**Deviations**

- `Stack::aggregate_state` keeps its ported behaviour (an idle tier-2 service counts as stopped). The broker filters idle lazy services out of the status instead.
- The stack tests count launches from the supervisor's log, not from the stand-in script: on the Intel Mac a freshly written script can miss a whole two-second start (the first-run hold noted below).

**Deferred**

- Model workers behind the gate, and a refusal on the wire (`insufficient_memory`): the model gateway, with the SDK's R2. **Done since:** #28.
- The other profile rows (model idle unload, apps kept loaded) have values but no consumer yet.
- Pausing dependents and the consumer-reported probe trigger, unchanged from below.

## 2026-11-14 (later) · Startup probe ramp; Mac results (isonomic-capsule PRs #1 to #3)

**Changed, with Omar's approval** (PR #3)

- While a service starts, readiness probes begin 50 ms apart and double up to the configured interval (5 s), instead of waiting the full interval after the first miss. This applies to the first start and to restarts. The spec's health line is updated to match.

| Measured on the Intel test Mac, real PostgreSQL 18.4 | Before | After |
| --- | --- | --- |
| Cold start | 5.0 s | about 0.2 s |
| Force-kill to healthy | 6.1 s | 1.8 to 2.7 s |

Recovery is now mostly the 1 s restart backoff plus PostgreSQL's own crash recovery.

**Found on the Mac and fixed**

- macOS holds the first run of a brand-new script for 9 to 15 s on the test machine. A ported test that waited a fixed 300 ms failed there; it now waits for the log line itself, up to 90 s.
- The log pump stopped without a trace on a read error. It now writes the error into the service's log.

**Verified**

- 75 tests on Linux and on the Intel Mac; CI green on Linux and macOS for PRs #1 and #2.
- One unexplained failure in one Mac run after the ramp change; it did not reproduce in six further runs.

**Deferred**, unchanged from below.

## 2026-11-14 · Ported and extended

**Built**

- `capsule-core`: service states and tiers, the Capsule directory layout, engine config, an injectable clock, the hardware trait and profiles.
- `supervisor`: service lifecycle, crash monitor, hang probe, circuit breaker, log sink and startup stack, carried over with behaviour unchanged and their tests ported.
- New: every child's environment is built from an allow-list (`ChildEnv`). The environment is cleared at spawn and only declared variables are set. The baseline is a fixed `PATH`, the C locale and UTC. A test proves database, loader and home variables set on the engine never reach a child.
- New: a service can listen on a Unix socket (`Endpoint::Unix`). Templates gained `{socket}` and `{socket_dir}`. Every probe runs over either kind of endpoint.
- The Capsule layout keeps socket paths under the OS limit: sockets live in `<root>/run`, or in a short per-user directory under `/tmp` when the root is too deep. The run directory is user-only and one owned by another user is refused.

**Deviations**

- `service.rs` is split into `service.rs`, `spawn.rs` and `monitor.rs` to keep every file under 20 KB. No behaviour change.
- Hardware profiles live in `capsule-core::hardware`, not under `supervisor` as section 4.1 lists. The brief puts the hardware trait in `capsule-core` and the two belong together.
- `ServiceSupervisor::addr()` is now `endpoint()`, and `start()` returns an `Endpoint` instead of a socket address.
- Not ported: the service definitions for the cache, object storage and proxy, the model supervisor, the model downloader, the RAM gate and the routing module. They are out of scope for Milestone 0 or dropped by the design.

**Deferred**

- Pausing dependents when a service degrades: tier 3 does not exist yet.
- The probe trigger "immediately when a consumer reports an error": needs the broker to accept such reports.
- The RAM gate: lands with the model gateway. **Done since:** #28, and reading macOS memory in #52.
