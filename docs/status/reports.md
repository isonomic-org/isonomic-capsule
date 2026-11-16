# Problem reports status

Newest first. The engine's problem reports: the panic hook, the service and model watchers, the log ring, and the SDK's shared crate. Moved here from `docs/status.md`.

## 2026-11-16 · Problem reports: each report has its own id

- **`SDK_REF` moves to isonomic-sdk `4947e43`** (UI `ecd21d1`). Format 1 gains an optional top-level `id`: 32 hex characters, made when the report is.
- **Every engine report now carries one.** The engine builds every report through `Report::new`, which makes the id. A receiver can drop duplicates by it.
- **Older files:** a report written before ids existed gets one from `isonomic_reports::id::report_id`, derived from its file name and the Capsule's id. That is the window's part, as the sender.
- **Checked:** the planted-content tests now also check that every report's id has the right form, and that two reports from one run differ.

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
