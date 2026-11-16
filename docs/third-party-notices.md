# What the bundled programs' licences ask of us

The app redistributes three programs: the database and the cache as builds we make from their pinned sources, and the model server as its project's own release. None of the three licences asks for source code to be shipped or offered. Each asks for its copyright notice and licence text to travel with every copy, which `NOTICES.txt` does.

| Program | Licence | What it requires when we redistribute it |
|---|---|---|
| The database (`postgres`, built from source) | PostgreSQL License | The copyright notice and the licence's paragraphs appear in all copies. |
| The cache (`valkey`, built from source) | BSD 3-Clause | A binary copy reproduces the copyright notice, the list of conditions and the disclaimer in its documentation or other materials. The contributors' names may not be used to endorse or promote the app without written permission. |
| The model server (`llama-server`, the project's release) | MIT | The copyright notice and the permission notice are included in all copies or substantial portions. |

How it is kept:

- Each sidecar names its licence file in `sidecars.json` (`licenseFiles`). `fetch-sidecars` reads it from the archive or source tree it installs and refuses an archive without one, so a licence never silently goes missing.
- After installing, `fetch-sidecars` writes `sidecars/NOTICES.txt`: each program's name, version, licence and source address, followed by its licence text in full.
- The app bundle ships `NOTICES.txt` in `Contents/Resources/` (the window's release step).
- We change none of the programs' sources; our builds differ only in build options (the macOS they target and the features left out).
