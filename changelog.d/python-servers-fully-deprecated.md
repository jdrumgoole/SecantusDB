### The Python servers are fully deprecated: no PyPI releases, no CI

SecantusDB no longer publishes to PyPI, and CI no longer tests the Python
MongoDB and PostgreSQL servers. The Rust servers are the product. They ship as
crates on crates.io and as prebuilt binaries on the `secantusdb-v*` and
`secantusd-pg-v*` GitHub releases. The releases already on PyPI stay where they
are.

CI on every push now runs one Linux job for lint, format and the tests that are
not Python-server tests, in place of about twenty runner jobs across Linux,
macOS and Windows. The Rust-server lanes are unchanged. pytest stays the test
runner.

#### Removed

- `.github/workflows/publish.yml`, `wheels.yml` and `rust-wheels.yml`: the PyPI
  publishing of the `SecantusDB` and `secantus-core` wheels.
- `.github/workflows/crash-repro.yml`, which reproduced a Python-server crash.
- `.github/workflows/record-durations.yml`, `ci/durations/` and the
  `pytest-split` dev dependency, which only balanced the old test shards.
- `[tool.cibuildwheel]` in `pyproject.toml`.
- `test.yml`: the `test` (Linux + macOS), `test-windows` and `test-durable`
  matrices, the engine-parity step of the `rust` job, and the Python SQL
  suites in `pg-oracle`.
- `validate.yml`: the 13 driver-gauge lanes against the Python MongoDB server
  and the 10 SQL-gauge lanes against the Python PostgreSQL server.
- `tests/test_release_push_race.py`, which tested the removed release helper.

#### Changed

- `invoke release`, `release-prepare` and `release-finalize` refuse, naming the
  Rust release tags instead.
- `test.yml`'s `test` job runs the files `tools/ci_test_selection.py` picks:
  every test file that does not import the `secantus` package (the ops board's
  modules excepted).
- `validation_summary.generate` takes `--server rust|python` (default `rust`),
  and the weekly summary is built from the Rust-server lanes.
