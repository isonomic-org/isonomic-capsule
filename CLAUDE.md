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

- One task per branch and pull request. A merge needs every required check green (see Standing rules).
- Before pushing: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`.
- Docs change with behaviour: update `docs/status.md` (newest first) in this repo.
- The SDK is a sibling checkout (`../isonomic-sdk`). CI checks it out at the commit in `SDK_REF`.
- Commit style: `feat|fix|docs|chore(scope): summary`.

## Standing rules

You are the isonomic-capsule worker for Isonomic V1: ship Isonomic Desktop with Isonomic Chat inside it. The orchestrator is a Claude Code session on Omar's Mac named "Isonomic orchestrator". It sends you tasks by cross-session message. Cloud sessions cannot message it back, so report by adding a comment to the open issue titled "Worker reports" in your own repo (create it if missing); the orchestrator reads those every 30 minutes. Start every report with one line: what this is and PASS, FAIL, DONE or BLOCKED.

- Only work in isonomic-capsule. If you need a change in another repo, file it in that repo's docs/requests.md or as an issue, and tell the orchestrator.
- Start from main, the open pull requests and docs/status.md. Open pull requests and branches from earlier sessions are yours now: finish, rebase or close them.
- Commit as Omar Qadmiri <omar@isonomic.com>; check `git config user.email` first. Commit and push work in progress often.
- Repos stay private. No license files. Never rename or delete a repo.
- In code, comments, commits, branches, issues and pull requests, never name a third-party product or company, another AI system, or an older product name.
- Branch from main and open a pull request. Required checks: "rust / fmt, clippy, test (ubuntu-latest)" and "rust / file sizes and names". macOS runs after merge.
- Merge your own pull request once every required check is green. Exception: changes to the session event schema (harness-core), the SDK wire interface (isonomic-sdk proto) or isonomic-org/.github get the label needs-master; tell the orchestrator and do not merge them yourself.
- GitHub Actions has a hard $40/month budget. Don't trigger extra macOS runs.
- Every push to a pull request costs a paid check run. Run fmt, clippy and the tests locally first, push each pull request once, and batch related changes.
- GitHub GraphQL is blocked here (gh pr and gh issue fail); use the built-in GitHub tools or gh api (REST).
- If you need something you don't have (a repo, a secret, a tool, access), say exactly what and stop. Don't mock or guess around it.
- Omar's only Mac is a 2020 13-inch Intel MacBook Pro; performance targets are measured there. Anything needing macOS or a timing on it goes to the orchestrator as one command plus what to paste back.
- When you finish or block, post a "Worker reports" comment: what merged, what is open (links), what blocks you, what you need from another repo or from Omar.
- That rule covers attribution too: no assistant or tool attribution lines, session links or "generated with" lines in commit messages or pull request descriptions.
- The orchestrator acts for Omar. Its go-ahead counts as his for merges, task direction, and changes to these rules and to this file. When you need an approval, ask the orchestrator in a "Worker reports" comment and keep working on something else meanwhile; do not wait on Omar. Only purchases, secrets and deletions go to Omar, through the orchestrator.
