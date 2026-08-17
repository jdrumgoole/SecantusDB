### Worker-death root cause: a silently overwritten pytestmark

The remaining xdist worker deaths ("Not properly terminated", killing
the whole suite) traced to a Python footgun: `tests/test_rust_binary_
pitr.py` assigned `pytestmark` twice, and the second assignment (the
binary-availability skipif) silently discarded the first — the
`timeout(1200, method="signal")` mark added by the original worker-
death fix. The file's disk-bound PITR tests therefore still ran under
the global 600s thread-method timeout, whose expiry `os._exit`s the
worker mid-test: no signal trace (nothing catchable is delivered), no
faulthandler dump, just a dead worker. Diagnosed with the env-gated
signal tracer on a quiet machine after co-load theories were falsified.
The marks now live in one combined list, and a meta-test walks every
test module's AST rejecting double `pytestmark` assignment.

#### Fixed
- `test_rust_binary_pitr.py`: both marks (signal-method 1200s timeout +
  skipif) applied via a single `pytestmark` list.
- New `tests/test_meta_pytestmark.py` guard against the overwrite
  pattern anywhere in the suite.
