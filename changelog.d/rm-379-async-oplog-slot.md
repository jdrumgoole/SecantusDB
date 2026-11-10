### A write on the asynchronous oplog says how long it held its oplog slot

mongod's slow-query line and profiler entry for a write carry
`totalOplogSlotDurationMicros`: the time from the write reserving its oplog
timestamps to its commit. The Rust MongoDB server reported it only on the
synchronous oplog. With `--oplog-async`, a write's entries are numbered only
after it commits, so nothing was timed and the field was missing. The server
now times each write from the point where it generates its oplog entries,
which is where mongod reserves them, to its commit. A transaction is timed
across its commit, which is where mongod reserves a transaction's slots.

#### Fixed

- On the asynchronous oplog, a write's `Slow query` line and profiler entry
  now carry `totalOplogSlotDurationMicros`, as mongod 8.2.11's do.
  - Each emit outside a user transaction records when it ran. The time from
    then to the write's commit is added to the operation's count.
  - A write that rolls back counts nothing, as on mongod.
  - A user transaction's `commitTransaction` counts the time of its commit.
    Its statements count nothing.
- The storage crate's slot tests (`crates/secantus-storage/tests/oplog.rs`)
  now run on both oplogs. Before this change they were pinned to the
  synchronous one.
- This is a change to the shared `secantus-storage` crate, which the
  PostgreSQL server also links. Only the asynchronous-oplog path changes,
  and the PostgreSQL server sets no option that selects it.
- Fixes RM-379 (tests/backlog/test_rm_355_364.py::test_an_async_oplog_write_says_how_long_it_held_its_oplog_slot)
- Filed RP-747: RP-202's cancel test was honoured up to 0.52 s late, against its 0.3 s budget, in this item's PG gate. It then passed 14 of 14 runs on two rebuilt commits.
- Filed RP-748: the local login comparison differs in 6 of 547 TLS CRL scenarios, because the reference PostgreSQL 15 now links OpenSSL 4.
