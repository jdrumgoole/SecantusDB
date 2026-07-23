### Ops Board: CI monitor and version drift

The Ops Board gains a **CI** page showing recent GitHub Actions runs across every
workflow, with the branch, event, state and a link through to the run. This is
the cross-session half of the board's tracking story: a run triggered by anyone —
your push, a parallel worktree's session, a cron, a release tag — appears here
without anything having to opt in, because GitHub is the shared source of truth.

The same page shows **version drift** for both independently-versioned servers:
what the working tree carries versus the most recent matching tag, for the Python
server (`vX.Y.Z`) and the Rust server (`secantusdb-vX`). That panel is read from
local files and `git tag` only, so it never depends on the network.

The GitHub layer degrades rather than breaks: if `gh` is missing, unauthenticated
or slow, the page still renders and explains what to do instead of failing.
Queries are bounded and briefly cached so a polling UI can't spawn a `gh` process
per tick.

#### Added

- `/ci` page: recent workflow runs (bounded, cached) plus per-server version
  drift.
- `secantus.opsboard.github`: read-only `gh` wrapper with an injectable runner,
  TTL cache, bounded limits and graceful degradation.
- `secantus.opsboard.versions`: local version + latest-tag reader for both
  servers (no network).
