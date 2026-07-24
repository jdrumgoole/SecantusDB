# Ops Board

The Ops Board is an optional local web app — a FastAPI app served behind
a [pywebview](https://pywebview.flowrl.com) window — that drives and
observes the **build → test → release** cycle for all three SecantusDB
servers: the pure-Python MongoDB-wire server, the Rust server, and the
PostgreSQL-wire server.

It's a **developer tool**, not a production console or a CI replacement.
It runs the project's own `invoke` tasks — it never invents build or
release mechanics of its own — and it serves over loopback behind a
per-launch token.

## Install

Like the [admin UI](admin.md), it lives behind an optional extra so the
base wheel doesn't carry a FastAPI / uvicorn / pywebview dependency
closure:

```bash
uv sync --extra opsboard
```

Static assets (HTMX) are vendored in the package — no CDN is contacted
at runtime.

## Launch

```bash
./inv opsboard                 # native window
./inv opsboard --no-window     # headless; open the printed URL
```

or the console script directly, which exposes the full flag set:

```bash
uv run --extra opsboard secantus-opsboard --help
```

Settings resolve **CLI flag > environment variable > saved config file >
default**. `--save` persists the resolved non-secret config (default
`~/.secantus/opsboard.json`); `--print-config` shows what would be used
and exits.

| Setting | Flag | Environment variable |
| --- | --- | --- |
| Bind host | `--host` | `SECANTUS_OPSBOARD_HOST` |
| Port (0 = free) | `--port` | `SECANTUS_OPSBOARD_PORT` |
| Repo it drives | `--repo-root` | `SECANTUS_OPSBOARD_REPO_ROOT` |
| Headless | `--no-window` / `--window` | `SECANTUS_OPSBOARD_NO_WINDOW` |
| Job journal | `--db-path` | `SECANTUS_OPSBOARD_DB` |
| Job logs | `--log-dir` | `SECANTUS_OPSBOARD_LOGS` |
| Config file | `--config` | `SECANTUS_OPSBOARD_CONFIG` |

The auth token is a secret and is deliberately **not** stored in the
config file: it comes from `--token`, `SECANTUS_OPSBOARD_TOKEN`, or a
generated-and-persisted `~/.secantus/opsboard-token`.

## Jobs: one runner for the UI and the CLI

The load-bearing idea is that the board doesn't own a private way of
running things. Both the UI *and* the `./inv` CLI spawn builds through
the same shared runner (`secantus.jobkit`), so a build you start in a
terminal and one you start in the browser are **the same journaled
process**.

Every `./inv <task>` is recorded in a small SQLite journal and has its
whole terminal output teed to a per-job log file, which the board tails
live. That means a build started from any terminal — or by another
session — shows up in the Jobs page with real progress, with nothing to
opt into.

Tracking stays build-free in an unsynced worktree because the wrapper
loads the runner by file path rather than importing the (WiredTiger-linked)
`secantus` package. To opt out for a single run:

```bash
SECANTUS_NO_TRACK=1 ./inv lint     # untracked
uv run inv lint                    # also untracked
```

### Progress

Task subprocesses expose no progress API, so progress is *derived* from
the log stream and is best-effort:

* a **determinate bar** from pytest's `[ NN% ]` markers (the test, perf
  and gauge tasks);
* a **phase stepper** from `==> [k/N] label` step markers, which the
  `py-gate` and `rust-gate` tasks emit — each sub-step lights up as it
  runs;
* an **animated indeterminate bar** plus elapsed timer when a task
  exposes neither.

The raw log is always available, collapsed beneath.

### Cancelling

**Cancel** (per job) and **Cancel all running** tear down the job's
entire process tree — escalating SIGINT → SIGTERM → SIGKILL across the
process group *and* any descendant that escaped it, such as a shell,
`uv`, `cargo`, or a `pytest` worker. Finished detached children are
reaped so they don't linger as zombies. Process-group teardown is POSIX;
the board targets macOS and Linux.

### Running now vs history

The Jobs page separates what's **running now** from the **history** of
finished jobs. The running block refreshes itself — quickly while work is
in flight, slowly when idle — and carries the cancel controls; the history
table below pages through completed jobs only, so a row can't drift
between pages as it finishes.

## Gauges

The **Gauges** page lists all thirteen driver-conformance suites —
pymongo, pymongo-async, Go, Node, Java, Kotlin, Ruby, Rust, PHP (library
and extension), C, C++ and C#/.NET — with a Run button for each server,
so a single gauge can be pointed at either the Python or the Rust server.
Each row shows the local toolchain that gauge requires and an expected
duration, and its info dialog explains what that particular suite proves.

Each row also shows **how the gauge last scored** — passed-of-ran and the
pass rate, green when clean and amber when anything failed, with the
report date beneath and the full passed/failed/errored/skipped breakdown
on hover. Those numbers are read from the report each gauge itself
generates (`docs/validation-report*.md`), so they can't drift from what
was measured; a gauge that has never been run here says so rather than
showing a misleading zero.

The dashboard additionally offers **All gauges** per server
(`validate-all`) with a parallelism field that sets `--jobs N`. Four or
fewer is recommended: above that, CPU contention makes timing-sensitive
gauges flake.

See [the conformance reports](validation-summary.md) for the published
numbers.

## Time estimates

Every activity carries an estimated duration. Because every run is
journaled with its duration, the estimate is the **median of that task's
past successful runs on this machine** — reflecting your hardware and
warm caches rather than a guess. Only successful runs count, since a
failure often aborts early and would skew the estimate low.

Until a task has completed successfully here, the board falls back to a
rough declared figure and says so explicitly. A guess is never presented
as a measurement.

## CI and version drift

The **CI** page shows recent GitHub Actions runs across every workflow,
with branch, event and state. A run triggered by anyone — your push, a
parallel worktree's session, a cron, a release tag — appears here with no
opt-in, because GitHub is the shared source of truth. It needs the `gh`
CLI installed and authenticated; without it the page explains that rather
than failing.

Local jobs and GitHub runs are also shown together in one **activity
feed**, newest first, with each row tagged `local` or `GitHub CI` — so
"did this build run on my machine or on CI?" is answered by the row
rather than by which page you opened. GitHub's states are normalised to
the same passed / failed / running words the local jobs use.

You can also **start** a CI run from here: pick a workflow that accepts a
manual dispatch and a ref, and the board triggers it on GitHub (it runs
there, not locally). Workflows that publish — to PyPI, or that cut
release binaries — are flagged and require their exact name typed as
confirmation, the same gate the [Release](#releases) page applies, since
dispatching one is just as outward-facing.

The same page shows **version drift** for the two independently-versioned
servers: what the working tree carries versus the most recent matching
tag (`vX.Y.Z` for the Python server, `secantusdb-vX` for the Rust
server). That panel reads local files and `git tag` only, so it never
depends on the network — run `git fetch` first if another session may
have pushed a tag.

## Releases

Releases are **irreversible and outward-facing**: `release-prepare`
pushes a tag that triggers publication to PyPI. The **Release** page
therefore leads with a readiness checklist rather than a button:

* on `main`;
* working tree clean (vendored-submodule drift is tolerated, exactly as
  `release-prepare` itself tolerates it);
* in sync with `origin`;
* a changelog fragment is pending in `changelog.d/`;
* recent CI on `main` is green — **advisory only**, and never blocking.

The policy is deliberately fail-safe: a blocking check must come back
definitively OK. A check that *cannot be verified* blocks just as a
failing one does, because "we couldn't tell" is not a good reason to
publish.

Starting a release then requires typing the **exact version** as
confirmation — not a checkbox, not the word "yes" — and any blocking
check requires an explicit override. As everywhere else, the board runs
the project's own `invoke` release tasks; it never tags or publishes by
itself.

## External processes

The Jobs page also lists build and test processes running on this machine
that were **not** started through `./inv`. These are shown honestly as
command and elapsed time only: the board didn't spawn them, so their
output belongs to whatever terminal did and there is no log to attach
to. Start a build with `./inv <task>` to get it fully tracked with a live
log.

Together these form three tiers of tracking: GitHub Actions for anything
remote, the shared journal for anything started via `./inv`, and a
process scan as the backstop for everything else.
