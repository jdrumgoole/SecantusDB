### wait_ci reports the number of checks it judged

`scripts/wait_ci.sh` printed the check count from the moment its first loop ended, which can come before later workflows' checks register: PR #2565's green verdict said "all 2 checks passed" of 10. It now counts them again before reporting. `scripts/wait_ci.sh` and its test also join the MongoDB-only paths that skip CI's `pg-oracle` lanes.

#### Fixed

- `scripts/wait_ci.sh`: the GREEN line's count is the final one (`tests/test_wait_ci.py`).

#### Changed

- `tools/ci_pg_needed.py`: `scripts/wait_ci.sh` and `tests/test_wait_ci.py` are MongoDB-only paths.
