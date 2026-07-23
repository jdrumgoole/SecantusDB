# SecantusDB Ops Board — plan

A standalone FastAPI + HTMX + pywebview app (`src/secantus/opsboard`, mirroring
`src/secantus/admin`) that drives **and** observes the **build → test → release**
cycle for the three server deliverables from one panel.

## Status

- **Phase 1 (skeleton) + Phase 2 (shared `jobkit` runner + journal + `./inv`
  repoint + jobs UI) — LANDED** on branch `opsboard` (2026-07-23). 27 tests
  green (`tests/test_jobkit.py`, `tests/test_opsboard.py`); ruff clean; real
  socket boot + `./inv` tracked/untracked paths smoke-verified.
  - `src/secantus/jobkit/` — stdlib-only `_core.py` (Journal + pty-tee
    `run_tracked`), loaded by `./inv` by file path (import-light invariant
    verified: `secantus` stays unimported).
  - `./inv` repointed through jobkit; `SECANTUS_NO_TRACK=1` is the untracked
    escape hatch; bare `uv run inv` untracked.
  - `src/secantus/opsboard/` — FastAPI + HTMX app (dashboard cards, jobs list
    with cursor pagination, job detail with live log-tail, cancel), token
    middleware, `invoke opsboard` task, `[opsboard]` extra, `secantus-opsboard`
    console script. Release-class tasks are disabled on the dashboard (they
    await the Phase 5 confirm-gated Release page); the server-side confirm gate
    is already enforced.
  - `opsboard/config.py` — layered config (`OpsboardConfig`): CLI flag > env
    var > saved JSON config (`~/.secantus/opsboard.json`, `--save`) > default.
    Every persistable setting has an env var (`SECANTUS_OPSBOARD_HOST` / `_PORT`
    / `_REPO_ROOT` / `_NO_WINDOW` / `_DB` / `_LOGS` / `_CONFIG`); token is a
    secret kept out of the file (`--token` / `SECANTUS_OPSBOARD_TOKEN` /
    `opsboard-token` file). `argparse` CLI adds `--host/--port/--repo-root/
    --window/--no-window/--db-path/--log-dir/--config/--save/--print-config`.
    `export_env()` propagates the journal/log locations to spawned `./inv`
    children so the whole host agrees on where the journal lives.
- **Phases 3–6 pending:** Build/Test pages (all gates + gauges data-driven),
  GitHub/PyPI observation (Tier 1 tracking), Release page + Tier 3 discovery,
  docs.

Decisions locked (2026-07-23):

- **Scope:** all three servers — Python `SecantusDBServer` (PyPI), Rust server
  (binary + `_secantus_server`), and `SecantusPGServer`.
- **Capability:** orchestrate **and** observe (run invoke tasks as subprocesses
  with streamed logs, *and* poll GitHub Actions / PyPI / releases).
- **Stack:** reuse the admin stack — FastAPI + HTMX + Jinja + pywebview. New
  package `src/secantus/opsboard`, `invoke opsboard`, `--extra opsboard`.

## 1. Managed targets

| Target | Version source | Build | Test | Release path |
|---|---|---|---|---|
| **Python server** (PyPI `SecantusDB`) | `pyproject.toml` + `__init__.py` `__version__` | `wheels.yml` (cibuildwheel) | `invoke py-gate` · `test` · `perf` · pymongo gauges | `release-prepare`→`release-finalize`, tag `vX.Y.Z`, `publish.yml` (OIDC) |
| **Rust server** (binary + `_secantus_server`) | 12× `Cargo.toml` `-beta.N` | `rust-build`, `rust-binary-build`, `rust-wheels.yml`, `release-binaries.yml` | `invoke rust-gate` · `rust-test` · `rust-parity` · gauges `--server rust` | `rust-bump`, tag `secantusdb-v*` |
| **PG server** (`SecantusPGServer`) | shares Python version line | (part of Python wheel) | `invoke validate-psycopg` · `validate-slt` | ships with Python package |

Rule that overrides everything: the app calls **only the sanctioned `invoke`
tasks**. It never shells out `git tag` / `uv publish` / `cargo publish` itself.

## 2. Architecture (mirrors `admin/`)

- **`opsboard/app.py`** — `create_app()` factory: `TokenAuthMiddleware`,
  `/healthz`, static, Jinja templates, routers. Same shape as `admin/app.py`.
- **`opsboard/cli.py` + `launcher.py`** — pywebview window (or `--no-window` for
  CI), reusing admin's launcher pattern.
- **`secantus/jobkit/`** — the shared, **web-free, stdlib-only** job runner that
  both the invoke CLI *and* the web app spawn through. Must never `import
  secantus` (that pulls WiredTiger and would break "lint/fmt in an unsynced
  worktree" — see memory `inv-in-worktrees`), so it is an *outer wrapper
  subprocess*, not an invoke `Executor` plugin. `python -m secantus.jobkit
  <task>`: opens a journal row, allocates a per-job logfile, `Popen`s the real
  `uv run --no-sync --with invoke python -m invoke <task>` under a **pty**, tees
  the pty output to both the terminal and the logfile, records status/exit on
  completion. One journal row per `inv` invocation.
- **`jobkit/journal.py`** — the shared job journal: sqlite at
  `~/.secantus/opsboard.db`. Records `{job_id, host_pid, worktree, target, task,
  argv, started_at, status, exit_code, log_path}`. **Paginated** list API
  (cursor/limit — never unbounded `to_list()`). Single source of truth for local
  jobs regardless of who started them (§4).
- **`opsboard/runner.py`** — the web app's `JobRunner` is a thin wrapper that
  spawns the *same* `python -m secantus.jobkit <task>` (plus `gh` subprocesses
  for the observation layer). Because CLI and UI use one entrypoint, a
  terminal-started and a UI-started job are indistinguishable in the journal.
  Supports cancel (SIGINT→SIGKILL).
- **`opsboard/github.py`** — thin `gh`/GitHub-API wrapper: workflow runs
  (`test.yml`, `validate.yml`, `publish.yml`, `release-binaries.yml`, …), PR
  checks (reuse `pr-watch` logic), latest tags, latest PyPI version. All list
  calls paginated.
- **`opsboard/discovery.py`** — best-effort process-table scan for uninstrumented
  local runs (§4, tier 3).
- **`opsboard/sse.py`** — reuse admin's `Sampler`/`Hub` pattern to push live log
  lines + status to the browser over SSE/WS.
- **`opsboard/registry.py`** — declarative catalog mapping each target → its
  build/test/release task names, so the UI is data-driven and a new gauge/target
  is a one-line addition (avoids the stale hardcoded-capability-table trap).

## 3. Pages / feature areas

1. **Dashboard** — three target cards: local version, latest git tag, latest
   PyPI/release version, ahead/behind state, last CI run, one-click gate button.
2. **Build** — trigger `rust-build` / `rust-binary-build` / `rust-server-build`;
   local artifacts; links to `wheels.yml`/`rust-wheels.yml` CI runs.
3. **Test** — run `test`, `py-gate`, `rust-gate`, `perf`; any driver gauge
   (dropdown of the 13 + `--server python|rust`), `validate-all`,
   `validate-all-servers`. Live streaming log + pass/fail/skip parse from report
   files.
4. **Release** — readiness checklist (changelog fragment present? tree clean? in
   sync with origin? gates green?), then a **confirm-gated** launch of
   `release-prepare`→`release-finalize` (Python) or `rust-bump`+tag (Rust). Poll
   `publish.yml`/`release-binaries.yml` to completion. Release actions require an
   explicit typed confirmation (irreversible + outward-facing).
5. **CI monitor** — live table of recent GitHub Actions runs across all
   workflows with drill-in to failed logs; PR check watcher.
6. **Jobs** — paginated history of every run (local + external) with re-run and
   full-log view; the live-tracking surface from §4.

## 4. Cross-session / CLI job tracking (the "connect to existing builds" ask)

Three tiers, honest about what each can and can't do. A process's live stdout
belongs to the terminal that spawned it — you **cannot** attach to the streamed
output of a run you didn't start and that wasn't instrumented. So:

- **Tier 1 — GitHub / CI (free, authoritative).** Any run triggered by anyone —
  a developer's `git push`, another Claude session, a cron, a release tag — is
  visible via the `gh` API with full progress and logs. This already covers
  remote *builds* (`wheels.yml`/`rust-wheels.yml`), *CI* (`test.yml`/
  `validate.yml`), and *releases* (`publish.yml`/`release-binaries.yml`). No
  instrumentation needed; this is the primary cross-session surface. (Phase 4.)

- **Tier 2 — Shared `jobkit` entrypoint (default-on, transparent).** Instead of
  opt-in instrumentation, make the shared runner the *default* path for invoke
  builds so CLI and UI runs are one code path:
    - Repoint the **`./inv` wrapper** from `python -m invoke` to
      `python -m secantus.jobkit` (a one-line `os.execvp` change). Every
      `./inv <task>` a developer or another session runs is now a journaled job
      with a tailable logfile — no flag to remember.
    - The web app's `JobRunner` spawns the *same* `python -m secantus.jobkit
      <task>`, so a terminal-started and a UI-started run are indistinguishable.
    - The UI attaches to any run — its own or a terminal's — by reading the
      shared journal and tailing the logfile over SSE (offset-based). No fd
      ownership problem: the `jobkit` wrapper, not the terminal, owns the
      process's stdout.
  Constraints: `jobkit` stays stdlib-only / import-light so the unsynced-worktree
  lint/fmt property holds; bare `uv run inv` remains an *untracked* escape hatch;
  colors/Ctrl-C survive because the wrapper drives the child under a pty. (Phase
  2 builds `jobkit` + journal + the `./inv` repoint.)

- **Tier 3 — Process discovery (best-effort, zero instrumentation).**
  `discovery.py` scans the process table for signature commands (`pytest`,
  `cargo build|test`, `cmake`, `inv <task>`, `uv run`, `secantusd-rs`). Shows
  them as **untracked external jobs**: cmdline, start time, cwd (via
  `lsof`/proc), elapsed. Cannot stream their stdout (fd owned by the other
  tty), but detects completion when the pid exits and can tail a known report
  file (`.validation/raw*.json`, build logs) if the task writes one. macOS-first
  (`ps`/`lsof`); good enough for this dev tool. Doubles as a way to spot leaked
  orphaned shells/processes (see memory `orphaned-claude-shells-eat-cpu`).

Summary: **CI/remote runs are trackable for free (Tier 1); local runs are fully
trackable when instrumented (Tier 2) and best-effort visible when not (Tier 3).**

## 5. Packaging & tooling

- Optional deps under `[project.optional-dependencies] opsboard`
  (fastapi/uvicorn/pywebview/httpx) — same "don't burden the base wheel" rule as
  `admin`.
- New `invoke opsboard` task (copy of the `admin` task shape: `--port`,
  `--no-window`, `--token`).
- Token auth on by default (persisted `~/.secantus/opsboard-token`), like admin.
- Dev-only surface; excluded from sdist/wheel if it carries heavy assets.

## 6. Testing

Real subprocess-runner tests (fake fast tasks; assert streaming/exit/cancel),
journal + jobs_store round-trip + pagination tests, `httpx.ASGITransport` route
tests mirroring admin's style, discovery tests against a spawned sentinel
process. GitHub layer tested against recorded `gh` JSON fixtures. No mock — the
runner drives real short-lived subprocesses.

## 7. Phased delivery (each a PR off a worktree)

1. **Skeleton** — package, `create_app`, token middleware, dashboard shell,
   `invoke opsboard`, `--extra opsboard`. (borrows admin wholesale)
2. **`jobkit` (shared runner) + journal + `./inv` repoint + SSE** — the stdlib
   pty-tee wrapper; run/stream/cancel one task end-to-end from both CLI and UI
   through one entrypoint; journal written from day one; paginated history.
3. **Registry + Build/Test pages** — all gates + 13 gauges wired data-driven;
   report parsing.
4. **GitHub/PyPI observation (Tier 1 tracking)** — CI monitor, version drift on
   dashboard, remote build/release progress.
5. **Release page + Tier 3 discovery** — confirm-gated `release-prepare/finalize`
   and Rust tag flow with workflow polling; `discovery.py` process scan as the
   backstop for raw `cargo`/`pytest` runs that skip `inv`. (Tier 2 default-on
   tracking already lands in Phase 2 via the `./inv` repoint.)
6. **Docs + polish** — Sphinx page, screenshots, README mention.

## 8. Key risks / constraints

- **Release safety:** call only vetted `invoke` release tasks; confirm-gate every
  outward-facing action.
- **Long-running jobs** (gauges, publish poll run 15–25 min): jobs run detached
  with persisted logs so a window close/reload doesn't kill them; reconnect
  re-attaches to the live log.
- **Parallel worktrees:** git state can shift between turns — surface current
  branch/HEAD, never assume, and scope task runs to a chosen worktree path.
- **No live stdout for uninstrumented external runs** — stated honestly in the UI
  (Tier 3 jobs are marked "external, log tail only").
