# Packaging: status

Newest first.

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

## 2026-11-15 · Packaging: the llama.cpp server ships

- `llama-server` is no longer deferred in `sidecars.json`, so the app bundles it and the gateway finds it at `sidecars/llama-server-<triple>/llama-server`.
- Pinned to llama.cpp `b12387`, the build isonomic-inference smoke-tested and benchmarked on the Intel Mac (the earlier pin, `b10331`, was never run against the models in the manifest). Hashes recorded from the release archives: arm64 `a654e602…`, x86_64 `624b91b4…`; 17 MB and 13 MB compressed. The whole directory ships (the server needs its `libggml*` and `libllama*` libraries beside it).

## 2026-11-15 · Every bundled service ships, and single programs are fused

**Built** (`cargo xtask bundle`, with the engine's install shapes from capsule #14)

- The app ships what the engine repo's `sidecars.json` bundles (not deferred, not draft), no longer PostgreSQL alone. A tree ships once per architecture as `<name>-<triple>/`, minus headers, readmes, stamps and build logs. A single program (the cache, built from source) is fused for every target into `<name>/<program>`, where the engine finds it when it has no per-architecture tree.
- Object storage stays out: deferred to Milestone 4 (Omar, 15 Nov), because its program is 134 MB (arm64) and 146 MB (x86_64) and stripping doesn't shrink it.

**Measured** (Intel Mac, one architecture)

| Part | Size |
| --- | --- |
| Window | 24.4 MB |
| Engine | 3.9 MB |
| PostgreSQL tree | 27.2 MB |
| Cache, one program | 3.8 MB |
| **The app** | **59.5 MB** (universal: about 113 MB) |

The cache build is 3.6 MB (x86_64) and 3.4 MB (arm64); the engine builds it from the pinned source on the Intel Mac for both architectures.

## 2026-11-14 (night) · The universal app (PR #4)

**Built** (`cargo xtask bundle` in this repo)

- `dist/Isonomic.app`, bundle id `com.isonomic.desktop`. The window and the engine are each built in release for `x86_64-apple-darwin` and `aarch64-apple-darwin` and fused with `lipo`. Both PostgreSQL trees ship under `Contents/Resources/sidecars/`; the engine picks the tree for its own slice, so the app needs no launcher logic.
- The engine repo must be cloned beside this one. The task runs its `cargo xtask fetch-sidecars`, which verifies every hash and architecture, cache hits included.
- From each PostgreSQL tree the app ships `bin` (`postgres`, `initdb`), `lib`, `share` and the two licence files. Headers and the upstream readme stay out.
- Signing is ad hoc: every Mach-O file inside, then the bundle. `codesign --verify --strict` must pass or the task fails.
- `--host-only` builds one architecture for quick local runs.

**Measured** (on the Intel test Mac)

| Part | Size |
| --- | --- |
| Window, both architectures | 42.4 MB |
| Engine, both architectures | 7.8 MB |
| PostgreSQL tree, Intel | 27.2 MB |
| PostgreSQL tree, Apple Silicon | 28.3 MB |
| **The app** | **106.0 MB** (one architecture only: 52.8 MB) |

- Leaving out the headers took each PostgreSQL tree from about 36 MB to 27 to 28 MB.
- First universal build from clean: about 75 minutes on the Intel Mac while other sessions were compiling (window: about 40 minutes for Intel, 26 for Apple Silicon; engine: 7 and 3). A rebuild with nothing changed takes seconds.
- `codesign --verify --strict` passes. `lipo` confirms both programs hold x86_64 and arm64.
- Launched from `dist/` the app runs the window, the engine and PostgreSQL from inside the bundle, and its Capsule goes to the default location under Application Support.

**Found on the Mac and fixed**

- Signing everything in one call failed: `codesign` refuses an app's main program while nested code is unsigned. Nested programs are now signed first and the bundle last.

**Deviations**

- The bundling task lives in this repo's xtask, not the engine's (the engine-supervisor spec lists `dist` there). The app's main program is the window, and the report that measures the app is here too.
- `LSMinimumSystemVersion` is 11.0, the first macOS that runs on Apple Silicon. The real minimum is open decision 1.
- No app icon: there is no approved design for one.
- `Cargo.lock` on `main` lacked the menu bar's dependencies because CI builds without `--locked`; this PR regenerates it.

## 2026-11-14 (later) · Fetch task and bundle lookup ported; local only

**Built** (`packaging` crate and `cargo xtask fetch-sidecars`)

- The pinned manifest is carried over as `sidecars.json`, with every URL and hash unchanged. Entries for services outside Milestone 0 are marked `deferred`: pinned, validated, not fetched.
- Every manifest rule is ported and tested: versions and licenses required, both Apple slices required, no universal target declared, https only, hashes well-formed, a missing hash fails closed unless the entry is a draft or the run is recording.
- The installer: download, verify the SHA-256 against the pin, unpack, move into place, then check the Mach-O architecture against the target. An install already in place is architecture-checked again. Nothing is unpacked before its hash matches.
- `keepInBin`: a directory install keeps only the listed programs. PostgreSQL keeps `postgres` and `initdb`, the two the engine runs.
- Modes: default, `--check`, `--record`, `--target=`.
- `Bundle`: finds bundled binaries at run time. An explicit directory wins; inside an app bundle it is `Contents/Resources/sidecars`; otherwise `sidecars` beside the engine. The engine never looks on the user's `PATH`.
- Pure Rust: no Node, no shelling out to `tar`.

**Verified**

- Hermetic tests with an archive built in memory.
- A real run from Linux: both Apple builds of PostgreSQL 18.4.0 downloaded, matched their pinned hashes and passed the architecture check (x86_64 and arm64). The Intel hash also matches the vendor's published `.sha256` file.
- A Linux build was added and pinned from the vendor's published hash, so the engine could be run with the PostgreSQL it fetched itself. It ran end to end: first-time setup, healthy, force-kill and restart, ordered stop.

**Measured**

- PostgreSQL tree after trimming `bin`: 36 MB (Intel), 37 MB (Apple Silicon). Both ship in a universal app, so about 73 MB before any further trimming. The tree still carries headers and other files the engine never reads.

**Deviations**

- Only the directory install shape is implemented. The single-file shape (with the fuse into a universal binary) and build-from-source validate in the manifest and are refused by the installer until a service that needs them is bundled.
- New manifest fields: `deferred` and `keepInBin`.
- The downloader uses the system trust store and proxy settings, so it also works behind a corporate proxy.

**Deferred**

- Trimming the tree further (headers, unused shared files): measure in the Milestone 0 report.
- The universal `.app` and ad-hoc signing. **Done since:** the window's release step (build 96 on the Intel Mac: both architectures, ad-hoc signed).

## 2026-11-14 · Not started
