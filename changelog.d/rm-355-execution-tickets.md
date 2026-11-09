### The global lock takes an execution ticket, and throughput probing sizes the pools

mongod bounds how many operations are inside its storage engine at once. Each operation takes an execution ticket as it takes the global lock: a read takes one from the read pool, a write from the write pool. When a pool has none free, the operation waits in that pool's queue. The Rust MongoDB server now does the same, ported from mongod 8.2.11's `TicketHolderManager` and `ThroughputProbing`. By default a monitor sizes the pools every 100 ms. It starts at twice the logical cores, split evenly between reads and writes (8 and 8 on an 8-core machine, as mongod gives), and then moves toward whichever size gives the best throughput. A concurrency given at startup fixes the pools instead, as on mongod. The concurrency cap is therefore on by default, where until now the server let every operation into the engine. `serverStatus.queues.execution` reports both pools and the monitor, and `globalLock.currentQueue` counts the operations waiting in them. A transaction returns its ticket between statements and takes one again with its next statement. If that ticket could be held by a request waiting on one of the transaction's own locks, it gives up with mongod's `LockTimeout`. This replaces the server's own opt-in cap on writes. `--write-tickets N` now fixes the write pool at N.

#### Added

- The sixteen execution control parameters, with mongod 8.2.11's defaults, types, coercion, bounds and messages, at startup (`--setParameter`) and, for the ones mongod allows, at runtime. These are `storageEngineConcurrentReadTransactions` and `storageEngineConcurrentWriteTransactions` (with their deprecated `wiredTiger*` aliases), the two queue depths, `storageEngineConcurrencyAdjustmentAlgorithm` and its interval, the seven `throughputProbing*` parameters and `delinquentAcquisitionIntervalMillis`. A concurrency set while throughput probing sizes the pools is refused with 20, after it was stored, as mongod refuses it.
- `serverStatus.queues.execution`: the write and read pools and the monitor, in mongod's order and types, ahead of `queues.ingress`.
- A ticket held longer than `delinquentAcquisitionIntervalMillis` is counted as delinquent in its pool's statistics and in the operation's own.

#### Changed

- Each global lock in IS or S takes a read ticket, and each in IX takes a write ticket, before the lock itself. X takes no ticket. A second global lock taken by the same operation uses the ticket it already holds. A ticket wait ends as mongod's does: a spent `maxTimeMS` gives 50, `killOp` gives 11601, and a full queue gives 433. An `awaitData` getMore returns its ticket while it waits for data.
- `globalLock.currentQueue` counts the readers and writers waiting for a ticket.
- `--write-tickets N` (and `[storage] write_tickets`) is now `--setParameter storageEngineConcurrentWriteTransactions=N`. The storage engine's own write admission (`secantus-storage`'s `admission.rs`) is removed.

#### Fixed

- Fixes RM-355 in part. Pinned by tests/mongod_live/test_execution_admission.py (`test_a_writer_past_the_pool_queues_for_a_ticket`, `test_set_parameter_coerces_and_refuses_as_mongod`, `test_the_parameters_have_mongods_defaults_and_settability` and three more) and by the lock manager's and `admission::execution`'s unit tests. RM-355's gap tests (`tests/backlog/test_rm_355_364.py::test_a_line_says_what_execution_control_admitted`, `tests/backlog/test_rm_001_028.py::test_slow_query_line_says_where_the_time_went`) wait on the per-command admission counts on the `Slow query` line.
