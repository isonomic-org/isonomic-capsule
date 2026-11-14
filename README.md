# isonomic-capsule

The Isonomic engine: a headless program that runs a user's Capsule (services, state, keys, backups). It keeps running when the window closes and is the same program a server node runs. The window talks to it over one Unix socket through `isonomic-sdk`.

Spec and status live in `isonomic-desktop/docs/architecture/`.

## Develop

The SDK is a sibling checkout: clone `isonomic-sdk` next to this repo. CI checks it out at the commit in `SDK_REF`.


```
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Tests are hermetic: stand-in binaries, fake protocol servers, injected clocks. No test needs a real service, the network or a particular machine.

Fetch the pinned PostgreSQL (never committed):

```
cargo xtask fetch-sidecars
```

A manual smoke test against a real PostgreSQL (it must not run as root):

```
cargo run -p supervisor --example pg_smoke -- <postgres bin dir> <scratch dir>
```
