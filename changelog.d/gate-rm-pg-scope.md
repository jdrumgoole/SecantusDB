### The MongoDB gate leaves the PostgreSQL server's tests to CI

The `ci-selected` step of `scripts/gate_rm.py` took about 100 seconds in some runs and 400 to 850 seconds in others. Its new timing log showed why. The slowest tests were all the Rust PostgreSQL server's: its corpora, rule trees and binary smoke tests. They run whenever the lane holds a built `secantusd-pg` and skip otherwise, and since 2026-11-09 both batch lanes hold one. The gate now leaves those test files out when only MongoDB-only paths changed, using the same rule (`tools/ci_pg_needed.py`) that lets CI skip its `pg-oracle` lanes. `--full` runs them anyway.

#### Changed

- `scripts/gate_rm.py`: the PostgreSQL server's test files (`tests/test_rust_pg*.py`, `tests/test_pgserver_embedded.py`) leave `ci-selected` when `tools/ci_pg_needed.py` says the change cannot reach them (`tests/test_gate_rm.py`).
