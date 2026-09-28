### The stale-artifact check now covers probes, gauges and benchmarks — not just pytest

PR #1586 added a collection-time check that refuses to run when a built artifact
was made from a different `crates/` tree than the checkout. It works, and it only
ever ran under pytest.

That left the gap where it matters most. This repo's primary bug-finding method is
not the test suite — CLAUDE.md says to run behaviour against the reference server
rather than reason about the source — and an ad-hoc probe, a driver gauge and a
benchmark all launch these artifacts directly, with nothing checking them. On
2026-09-28 a probe of `secantusd-pg` duly ran against a binary built from a
different tree, and the only thing that caught it was a hand-read of `--version`:
exactly the state #1586's own docstring complains about, where "the evidence was
sitting in `--version` output that no automated thing read".

#### Changed

- The comparison, the rebuild commands and the per-artifact cost strings moved
  from `tests/conftest.py` into `tools/provenance.py`, importable by anything that
  runs with the repo root on `sys.path`. `conftest.py` is now one caller rather
  than the owner; its behaviour is unchanged.
- Both probe launch paths check before starting a server: `tools/probes/_servers.py`
  (the embedded `_secantus_server`) and `tools/probes/pg_differential.py`
  (`secantusd-pg`). A stale artifact aborts the probe instead of producing
  divergences that are the artifact's age rather than the server's behaviour.
- The Windows `.exe` fallback moved into the shared `resolve_binary`, so the
  fourth caller cannot forget it. Getting it wrong once already made the guard
  watch a filename that cannot exist on Windows, and made 1,194 pgserver tests
  skip.

#### Added

- Eight tests in `tests/test_build_provenance.py`, including two that pin the gap
  this closes: each probe launcher must call the check, and `tools.provenance` must
  import with nothing but the repo root on the path — no pytest, no conftest.

#### Note

`conftest.py` discovers `tools/provenance.py` by walking up from its own
location and **abstains when it is not there**, rather than importing it
unconditionally. A verbatim copy of `conftest.py` is loaded from a temp
directory by `tests/test_crash_stall_watchdog.py` (deliberately, so the nested
session exercises the real watchdog), where no checkout sits above it — a hard
module-scope import failed to LOAD the conftest there, which is not one test
failing but every test in the lane. Pinned by
`test_a_copied_conftest_still_loads`.
