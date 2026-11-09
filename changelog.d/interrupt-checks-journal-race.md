### The interrupt-check comparison allows for mongod's journal race

`tests/mongod_live/test_interrupt_checks.py` compares the `numInterruptChecks` this server prints on a `Slow query` line with mongod's. For a write with `j: true`, mongod's own count depends on a race. If its journal flusher thread finished the round before the write waited on it, mongod skips the wait's two checks (`SharedStateBase::wait` in 8.2.12). Linux mongod in CI did that once (17 checks against 19). This server flushes on the request thread and always counts the wait. For the two `j` scenarios, the test now looks for this server's count in up to five of mongod's runs instead of requiring it to match one.

#### Fixed

- `tests/mongod_live/test_interrupt_checks.py::test_a_write_line_counts_mongods_interrupt_checks[update matching nothing, j]` / `[insert, j]` no longer fail when mongod's flusher wins the race (RM-360's test).
