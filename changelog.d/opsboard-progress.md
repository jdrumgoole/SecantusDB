### Ops Board: graphical job progress + full-tree cancel

The Ops Board's job view is no longer a wall of raw terminal output. Each run
now shows an **overall progress bar**, a **phase stepper** for multi-step tasks,
an elapsed timer, and a status-coloured bar — with the raw log tucked into a
collapsible panel underneath. Progress is derived from the log stream: the
pytest ``[ NN% ]`` markers drive a determinate bar for the test/gauge tasks, and
explicit ``==> [k/N] label`` step markers (now emitted by ``py-gate`` and
``rust-gate``, which also makes their CLI output clearer) light up the stepper's
sub-phases. Tasks that expose no signal get an animated indeterminate bar and
spinner.

Cancelling is now a real teardown. The per-job **Cancel** button — and a new
**Cancel all running** control — stop the job's entire process tree, escalating
SIGINT → SIGTERM → SIGKILL across the process group and any descendant that
escaped it (a shell, `uv`, `cargo`, or `pytest` worker), so nothing is left
running. The runner also reaps finished detached children so they don't linger
as zombies.

#### Added

- `secantus.opsboard.progress`: log→progress parser (phase markers + pytest %)
  driving an overall bar + phase stepper in the job view.
- `Cancel all running` control; per-job cancel now tears down the whole process
  group + escaped descendants (SIGINT→SIGTERM→SIGKILL) and reaps children.
- `py-gate` / `rust-gate` emit `==> [k/N] label` phase-step banners.
