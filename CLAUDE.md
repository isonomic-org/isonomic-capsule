# isonomic-capsule

Headless engine for Isonomic Desktop. A Rust workspace. The spec is `isonomic-desktop/docs/architecture/`, one folder per area with `spec.md` and `status.md`.

## Rules

1. **Hermetic tests only.** Injected clocks and hardware, stand-in binaries, fake protocol servers. No test may depend on free memory, real timers, the network or an installed service.
2. **Allow-listed child environments.** Every child gets a `ChildEnv`. Nothing is inherited from the engine's own environment.
3. **Persist, then publish.** Write the registry before notifying any subscriber.
4. **The engine owns ports and paths.** Nothing hardcodes one. Services listen on Unix sockets inside the Capsule directory where they can.
5. **Probes speak the real protocol.** No probe shells out to a client tool.
6. **Small files.** Every source and doc file stays under 20 KB. Split by responsibility.
7. **Names.** `ISONOMIC_` for environment variables, `com.isonomic.` for identifiers. No other product, repo or account name anywhere: code, comments, commits, branches, issues, docs.
8. **Binaries are never committed.** Bundled services are downloaded, pinned and verified.
9. **The engine knows nothing about windows.**
10. **Ported code keeps its behaviour and its tests.** A behaviour change to a ported module needs Omar's approval first.

## Workflow

- One task per branch and pull request. A merge needs green checks and Omar's approval.
- Before pushing: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`.
- Docs change with behaviour: update the area's `status.md` (newest first) in `isonomic-desktop`.
- The SDK is a sibling checkout (`../isonomic-sdk`). CI checks it out at the commit in `SDK_REF`.
- Commit style: `feat|fix|docs|chore(scope): summary`.
