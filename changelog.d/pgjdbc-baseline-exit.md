### The pgjdbc weekly lane's red now means something

The weekly pgjdbc gauge returned gradle's raw exit code, and gradle exits
non-zero while any test fails — so with ~200 documented standing failures
the lane was red by construction and its conclusion carried no signal. The
lane now compares the run's failures against a committed baseline
(`pgjdbc_validation/baseline.json`, seeded from the latest weekly run) and
fails only on regression: a failing test the baseline doesn't list, or a
parameterized test failing more times than recorded. Runs with fewer
failures stay green and print the newly-passing entries so the baseline can
be tightened (`python -m pgjdbc_validation.baseline --update`).

#### Changed
- `pgjdbc_validation/runner.py` exits by baseline comparison, not gradle's
  raw code; a gradle failure that produced no test results at all is still
  a hard failure, and a truncated run still refuses a verdict (124).

#### Added
- `pgjdbc_validation/baseline.py` (compare / verdict / `--update` CLI) and
  the committed `baseline.json` (204 standing failures, 2026-08-11 weekly).
