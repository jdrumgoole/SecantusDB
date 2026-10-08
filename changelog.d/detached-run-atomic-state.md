### `scripts/detached_run.py`: a stopped run always reads as finished

On Windows CI, `status` could crash on every call after `stop`, so a stopped
run never reported "finished". The supervisor could be killed while writing
its child's exit code, leaving that file empty.

#### Fixed

- The exit file and the state file are written beside their name and renamed
  into place, so a kill or a concurrent reader never sees one half written.
- A missing, empty or garbled exit file reads as exit code -1 instead of
  raising.
- `stop` records the run's end itself instead of leaving it to the next
  `status`.
