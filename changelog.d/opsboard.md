### Ops Board: a local web app to drive the build/test/release cycle

SecantusDB now ships an optional **Ops Board** — a local FastAPI + pywebview app
that drives the build, test, and release cycle for all three servers (the
pure-Python MongoDB server, the Rust server, and the PostgreSQL-wire server)
from one panel. It mirrors the admin app's stack: a loopback-only, token-gated
window with an HTMX front end, launched with `invoke opsboard` (or the
`secantus-opsboard` console script) behind the new `opsboard` extra.

Underneath it is `secantus.jobkit`, a shared, stdlib-only job runner that both
the Ops Board *and* the `./inv` CLI spawn through — so a build a developer
starts in a terminal and a build the UI starts are the same journaled process.
Every `./inv <task>` is now recorded in a small sqlite journal and has its whole
terminal output teed to a per-job logfile, which the board tails live; a build
started from any terminal or another session shows up in the board with real
progress. Tracking stays build-free in unsynced worktrees because the wrapper
loads the runner by file path without importing the WiredTiger-linked package
(`SECANTUS_NO_TRACK=1` opts out). This first slice lands the dashboard, the
paginated job history, per-job live log tail and cancel, and a layered config
(`CLI flag > env var > saved JSON > default`) with a full `argparse` surface.

#### Added

- `secantus.jobkit`: shared job runner + sqlite journal (cursor-paginated) with
  a pty-tee `run_tracked`; the `./inv` wrapper routes through it (import-light,
  `SECANTUS_NO_TRACK=1` to bypass).
- `secantus.opsboard`: FastAPI + HTMX + pywebview app — dashboard cards per
  server, job history, live log tail, cancel; token middleware; `invoke
  opsboard` task; `opsboard` extra; `secantus-opsboard` console script.
- `secantus.opsboard.config`: layered configuration (CLI > env > saved
  `~/.secantus/opsboard.json` > default) with an env var for every persistable
  setting and `--save` / `--print-config`.
