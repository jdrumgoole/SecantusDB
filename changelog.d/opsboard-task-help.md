### Ops Board: per-task explanations and time estimates

Every activity on the Ops Board dashboard now carries an info button that opens
a dialog explaining, in detail, what that task actually does — what the pymongo
gauge measures, which sub-steps the pre-commit gates run, why the perf suite is
the only one on in-memory storage, what the thirteen driver gauges cover and
which toolchains they need, and which tasks are irreversible release-class
operations. Each dialog also shows the exact `./inv` command it will run.

Alongside it is a time estimate — and it gets better the more you use the board.
Because every run is journaled with its duration, the estimate is the **median
of that task's past successful runs on this machine**, which reflects your
hardware and warm caches rather than a guess. Until a task has completed
successfully here, the board falls back to a rough declared figure and says so
explicitly, so a guess is never presented as a measurement.

#### Added

- Per-task info dialogs on the dashboard with long-form detail, the exact
  command, an irreversibility warning for release-class tasks, and a time
  estimate.
- `secantus.opsboard.estimates`: median-of-past-successful-runs estimation with
  an explicit `measured` / `rough` / `unknown` provenance shown in the UI.
- `Journal.completed_durations()`: bounded, exact-argv duration history
  (successful runs only, so an early-aborting failure can't skew the estimate).
