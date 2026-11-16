### Rust MongoDB server: `abortTransaction` waits for its write concern, as mongod's does

An `abortTransaction` now waits for the journal under its write concern, as
a `commitTransaction` does and as mongod 8.2.11's abort does. Its `Slow query`
line therefore counts mongod's three execution admissions (it counted two)
and the global IS of that wait in its `locks`.

#### Fixed

- `abortTransaction`'s line counts three execution admissions, as on mongod.
  Fixes RM-355 in part (tests/backlog/test_rm_355_364.py::test_ending_a_transaction_takes_mongods_execution_tickets).
  The item stays open; its `awaitData` getMore gap now has its own gap test,
  which measured 24,845 admissions against mongod's 4 for a 500 ms wait.
