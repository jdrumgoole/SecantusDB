---
name: build-and-test
description: Building and testing SecantusDB's Rust crates and its test suite on this machine. Fires when building a Rust server or extension, running `./inv rust-*` tasks, `cargo fmt` / `clippy` / `test`, touching the vendored WiredTiger or a `cmake/patch_wt_*.py` patcher, the PGO profile, `./inv sync`, the pytest suite's storage modes (fast vs durable), or a long run that keeps getting killed. Holds which crates the workspace commands skip, the invoke task for each job, the WiredTiger vendoring and patch rules, PGO, the test-storage modes and what they leave uncovered, and how to run something long without the session killing it.
---

# Building and testing

For setting up a worktree and its venv, and for the landing sequence, read
the `batch-worktree` skill first; for the Rust PostgreSQL server's own
build, `rust-pg-server`. Machine budget (CLAUDE.md): at most two building
agents per session, each with `CARGO_BUILD_JOBS=3`.

## Rust: the workspace skips the crates that matter most

`crates/Cargo.toml` EXCLUDES every crate that links WiredTiger:
`secantus-wt`, `secantus-storage`, `secantus-storage-py`,
`secantus-storage-adapter`, `secantus-server-py`, `secantusdb`,
`secantus-pgwire`, `secantus-pg`. So `cargo fmt` / `clippy` / `test` run
from `crates/` **silently skip them**: run each from its own directory, or
use the invoke tasks, which do.

| job | task |
| --- | --- |
| everything a Rust-server PR must pass (fmt, clippy `-D warnings`, tests) | `./inv rust-gate` |
| the clean workspace only | `./inv rust-test` |
| one WiredTiger-linked crate | `./inv rust-wt-test`, `rust-storage-test`, `rust-adapter-test`, `rust-pgserver-test` |
| one test | `./inv rust-test-one --crate <dir> --test <file stem> --name <test>` |
| fmt everything, WiredTiger crates included | `./inv rust-fmt` |
| the vendored WiredTiger static lib | `./inv rust-wt-build` |
| `secantusd-rs` (debug, what gap tests and probes use) | `./inv rust-binary-build` |
| the embedded `_secantus_server` extension, into the venv | `./inv rust-server-build` |
| `secantusd-pg` | `./inv rust-pgserver-build` |
| a pymongo repro script against a fresh binary | `./inv rust-repro` |
| concurrent-WiredTiger stress (must not panic) | `./inv rust-stress` |
| engine parity suites | `./inv rust-parity` |

CI runs fmt AND `clippy -D warnings` AND the full `cargo test`: a green
`cargo build` proves none of them. Before pushing `.rs` changes, run the
gate that covers the crates you touched.

Two traps with binaries: on macOS, `cp` over a running or recently run
binary's inode can get the new copy SIGKILLed (exit 137, no message) --
`rm` first, then copy. And a rebase or pull while a build runs leaves a
binary built from neither tree; the provenance check will refuse it.

## WiredTiger

- WiredTiger is **vendored** as a git submodule at `vendor/wiredtiger` (mongodb-7.0.33). The CMake build is driven by `CMakeLists.txt` (scikit-build-core + ExternalProject) and produces self-contained binary wheels via `cibuildwheel` for **cp310–cp313** on macOS arm64, **manylinux_2_28** + musllinux_1_2 x86_64/aarch64, and Windows AMD64 (read off the published 0.6.0b17 wheel list on 2026-09-27 — this line previously said cp312+cp313 / manylinux2014, which had drifted). macOS x86_64 is intentionally absent (runner-pool scarcity, Apple Silicon is the active target). `pip install SecantusDB` ships pre-built on supported platforms; users never need `cmake`/`ninja`/`swig`. The `cmake/patch_wt_*.py` scripts apply small idempotent patches to the vendored WT tree at `PATCH_COMMAND` time (off64_t→off_t for musl, Python module SUFFIX/dynamic_lookup, etc.); fix WT-side incompat by extending one of these patchers, not by editing the submodule directly. **Two patchers change BEHAVIOUR rather than the build**: `patch_wt_dsync_group.py` (2026-10-07, Joe's call) makes concurrent `method=dsync` commits share log writes (only the PostgreSQL server on macOS uses dsync), and `patch_wt_fsync_group.py` (2026-10-09) credits one `fsync` under `method=fsync` with every commit already written, which every server that syncs per commit off macOS runs. A process kill cannot test the second one, because the page cache survives it: its test is a hard reboot under load (`tasks/backlog-rust-mongodb.md`, the Linux entry). Three builds apply the patches and must agree — the wheel's CMake (`apply_wt_patches.cmake`), the crate's bundled copy (`scripts/wt_sys_refresh.py`, then commit `wiredtiger.sha256`) and `./inv rust-wt-build` (which reads the crate's list) — so a new patcher goes in the first two lists.

## Profile-guided optimisation

- **The Rust server is built with PGO** (profile-guided optimization) on top of thin-LTO + mimalloc (~12–19% on write/aggregate paths; `tasks/rust-perf-findings.md` Finding 8). Two halves: the **standalone `secantusd-rs` binary** builds two-stage per-arch in `.github/workflows/release-binaries.yml` (on-target profile via `bench/pgo_workload.py`); the **wheel-embedded `_secantus_server` extension** consumes a committed profile at `crates/pgo/_secantus_server.profdata.tar.gz` (CMake extracts it and passes `-Cprofile-use`). Regenerate the committed profile with `invoke rust-pgo-refresh` when the Rust hot paths change materially — it's only a hint (unmatched functions ignored, so a stale/cross-arch profile is safe, never build-breaking). `SECANTUS_PGO_DISABLE=1` turns it off; `SECANTUS_PGO_GENERATE=<dir>` switches the extension build to instrumentation (used by the refresh task). Don't delete the committed `.tar.gz` — it's the wheel's profile, not a stray artifact. **The Rust PostgreSQL server's released binary has fat LTO and a profile too (2026-10-13)**: the LTO is the crate's `[profile.release]`, so every release build and `cargo install` has it, and the profile is collected at release time by `scripts/build_pg_release.sh` from `bench/pgo_workload_pg.py`. Nothing is committed for it. A release build of that crate takes about nine minutes here.

## The Python test suite

- `pytest` with `pytest-xdist` parallel by default (`addopts = "-n auto"`). Tests must use `port=0` and a unique `storage_path` per test — pytest's `tmp_path` fixture gives both isolation and automatic cleanup. The default suite runs against real on-disk WiredTiger (schema / tables / B-tree / within-session behaviour all exercised for real) but in **fast test-storage mode**: `tests/conftest.py` sets `SECANTUS_TEST_FAST_STORAGE=1`, which makes `Storage` / `SecantusDBServer` default to `durable=False` (journal on, close-checkpoint skipped). This removes the per-instance close-checkpoint fsync that serialised across xdist workers (see `tasks/test-performance-plan.md`). **The checkpoint-durability path (crash-/reopen-durability, PITR, backup) is therefore NOT covered by the default local suite** — it is covered by (a) fixtures that pass `durable=True` explicitly (persistence / reopen / PITR / backup) and (b) the CI **`SECANTUS_FORCE_DURABLE=1` full-suite lane** (`.github/workflows/test.yml`), which forces full journal + checkpoint durability everywhere and runs the whole suite that way on every push. To reproduce that locally: `SECANTUS_FORCE_DURABLE=1 uv run python -m pytest`. The shipped server is unaffected — `durable` defaults to durable when no test env is set. (`tests/test_storage.py` has explicit `Storage` / `SecantusDBServer` reopen-roundtrip tests.) The perf-regression suite (`tests/test_perf_regression.py`) is the only test file that stays on `:memory:` storage, because the gates compare against fixed in-memory baselines where on-disk variance would invalidate the thresholds.
- To run a single test serially: `uv run python -m pytest -n0 tests/path::test_name`. The `-p no:xdist` form fails because `addopts` still injects `-n auto`.
- **Never a mock storage** (CLAUDE.md, Tooling): tests drive the real
  WiredTiger-backed `Storage(str(tmp_path))`.

## Syncing the venv

- **Sync with `./inv sync` (`uv sync --all-extras --reinstall-package secantus-core`), never a bare `uv sync --extra dev`.** Two ways a partial or cached sync produces a green run that proved less than it looked: (a) without the `rust` extra there is no `_secantus_core`, so the ~1700 engine-parity tests **do not collect at all** — they `importorskip` it deliberately, the suite still exits 0, and only comparing the collected count (~8,770 at the end of 2026-08) catches it; a partial sync also *prunes*, and once removed the whole Pelican toolchain the website tasks need. (b) `uv sync` does **not** rebuild `secantus-core` when its Rust source changes — it is a path dependency and uv reuses the cached build, so pulling a commit that touches `crates/secantus-core*` leaves the venv comparing current Python against **stale Rust**. That was seen as 31 phantom `test_rust_diff_parity.py` failures on a green `main`, which reads exactly like a regression. `--reinstall-package` busts it.

## Long runs

- **Long runs go through `scripts/detached_run.py`, not a bare background shell.** A supervisor that reaps the shell it spawned signals the whole process group, which killed three multi-minute runs in one session (a full suite twice at ~90%, once at 32%) with no OOM and no test failure — just `[killed]`. The helper starts the command with `start_new_session=True` so it leads its own process group and survives:
  `uv run python scripts/detached_run.py start --name suite -- python -m pytest -q`, then `status --name suite --tail 20` / `wait --name suite --timeout 3600` / `stop --name suite`. State (pid, argv, log, exit code) lands in `.detached-runs/<name>.{json,log}`. `tests/test_detached_run.py` pins the survival property by signalling the launcher's own group.

Long waits from a session belong in a background task, never a foreground
loop, which hits the 10-minute tool cap. Poll CI over REST with a sleep;
tight `gh pr checks` loops exhaust the GraphQL quota.
