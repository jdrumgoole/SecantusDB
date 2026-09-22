### `detached_run stop` says when it fails to stop anything

On Windows the stop path ran `taskkill` with its output sent to `DEVNULL` and
its exit code ignored, then printed `stopped <name>` and returned 0 whatever
happened. A kill that failed left no trace at all, and a caller that believed
the success and started a replacement would get two of whatever it was
running.

`stop` now captures what `taskkill` said, retries once if the process is still
there after ten seconds, and — if it is still alive after that — reports the
failure with the diagnostics and exits non-zero instead of claiming success.
The POSIX path already escalated to `SIGKILL`; it now verifies that worked too.

Prompted by `test_stop_ends_a_running_command` failing once on Windows CI and
passing on a re-run, with nothing in the log to say why. That test now prints
what `stop` and `status` reported when it fails.

#### Fixed

- `scripts/detached_run.py`: `cmd_stop` reports a kill that did not work.
- `tests/test_detached_run.py`: the assertion carries the evidence.
