### The MongoDB gate runs the storage tests on the asynchronous oplog too

CI's `rust-storage` job runs the storage crate's tests twice. The second run uses the asynchronous oplog (`SECANTUS_OPLOG_ASYNC=1`, `SECANTUS_OPLOG_NONLOGGED=1`). `scripts/gate_rm.py` ran only the first, so RM-361's oplog-slot timing passed the local gate and failed only in CI. The gate now runs that second pass too, with the environment CI's step sets.

#### Fixed

- `scripts/gate_rm.py`: a `storage-async-oplog` step runs `cargo test` in `crates/secantus-storage` under CI's async-oplog environment. A test pins that environment to `test.yml`.
