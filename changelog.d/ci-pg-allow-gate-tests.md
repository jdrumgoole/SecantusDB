### The batch scripts' own tests no longer pull in the PostgreSQL lanes

The tests of the MongoDB gate and of the batch runner (`tests/test_gate_rm.py`, `tests/test_batch_scripts_exit.py`) are now on the MongoDB-only allow-list, so a change to them alone no longer runs CI's `pg-oracle` lanes. A snapshot of RM-357 ran them for a one-file fix to the gate's test (2026-11-09).

#### Changed

- `tools/ci_pg_needed.py`: two more MongoDB-only paths (`tests/test_ci_pg_needed.py`).
