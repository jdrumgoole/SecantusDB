### `detached_run.py`: an exit that came before the state file

#### Fixed

- A command started with `scripts/detached_run.py start` that ended at once
  could end before its state file was written. Its exit was then recorded
  nowhere a reader of `<name>.json` would see it, and the file said
  `exit_code: null` until `status` or `wait` was called. `start` now records
  an exit that is already there.
