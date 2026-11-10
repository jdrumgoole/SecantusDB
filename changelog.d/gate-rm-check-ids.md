### The MongoDB gate refuses a Backlog-ID nobody reserved

`scripts/gate_rm.py` now begins with `tools/backlog.py check-ids --file backlog-rust-mongodb.md`, the check the PostgreSQL gate gained in #2583. It refuses an RM ID that the work adds unless the work reserved it, or another origin branch already holds the identical item. A hand-numbered ID used to meet its twin only on a snapshot PR, a CI cycle later.

#### Changed

- `scripts/gate_rm.py`: a `backlog-ids` step (`tests/test_gate_rm.py`).
