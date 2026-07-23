### Ops Board: local vs CI at a glance, startable CI runs, and a running/history split

Three things that make the board easier to read at a glance.

Builds now say **where they ran**. The CI page carries a merged activity feed of
local jobs and GitHub Actions runs, newest first, each tagged `local` or
`GitHub CI` — so "did this run on my machine or on CI?" is answered by the row
rather than by which page you happened to open. GitHub's status vocabulary is
normalised to the same passed/failed/running words local jobs use, so one set of
badges covers both.

CI runs can also be **started** from the board, for any workflow that accepts a
manual dispatch. Workflows that publish to PyPI or cut release binaries are
flagged and require their exact name typed as confirmation — the same gate the
Release page applies, because dispatching one is just as outward-facing as
running the release task locally.

Finally the Jobs page separates **Running now** from **History**. The running
block refreshes on its own (quickly while work is in flight, slowly when idle)
and carries the cancel controls; history pages through finished jobs only, so
rows no longer drift between pages as they complete.

#### Added

- `secantus.opsboard.activity`: merged local + CI feed with an explicit origin.
- `GitHubClient.workflows()` / `.dispatch()` and a confirm-gated `/ci/dispatch`.
- `Journal.list(include_running=False)` and a self-refreshing running block.
