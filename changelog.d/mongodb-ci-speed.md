### A MongoDB-only change no longer waits on the PostgreSQL lanes

A pull request that touches only the Rust MongoDB server's own code and tests -- its command layer, binary, storage adapter, wire layer, backlog and backlog tests -- now skips CI's three `pg-oracle` shards, which check the Rust PostgreSQL server against a real PostgreSQL and were the slowest job of the run at about 30 minutes. The decision is an allow-list (`tools/ci_pg_needed.py`): any file it does not name, shared crates included, still runs them, and pushes to `main` always do.

The local MongoDB gate got lighter in the same way. `scripts/gate_rm.py` runs the WiredTiger storage crate's two test runs (about nine minutes together) only when a change touches the storage crates, the WiredTiger build or the vendored engine, and a new `--only` option re-runs just the failed steps of a gate while still requiring every recorded step to be green. The batch lander no longer holds a green snapshot because the advisory `claude-review` job failed on its own setup.

#### Changed

- CI: a new `changes` job decides per pull request whether `pg-oracle` runs (`tools/ci_pg_needed.py`, `tests/test_ci_pg_needed.py`).
- `scripts/gate_rm.py`: `storage-test` and `storage-async-oplog` are recorded as skipped unless the diff from `origin/rm-batch` / `origin/main` touches `crates/secantus-storage/`, `crates/secantus-wt/`, `crates/secantus-wiredtiger-sys/`, `cmake/` or `vendor/wiredtiger`; `--full` forces them; `--only a,b` re-runs named steps (`tests/test_gate_rm.py`).
- `scripts/land_batch.sh`: a snapshot whose only failed check is `claude-review` is merged.
