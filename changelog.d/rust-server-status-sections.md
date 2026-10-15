### The Rust MongoDB server's `serverStatus` reports real counters

`serverStatus` now carries `asserts`, `mem`, `extra_info`, `globalLock`,
`logicalSessionRecordCache`, `opLatencies`, `security`, `transactions`,
the WiredTiger engine's own statistics (`wiredTiger`), and
`metrics.commands` / `document` / `operation` / `query`. Each counter was
measured against mongod 8.2.11 by running the same operations on both and
comparing what moved. A section with nothing true to report is left out,
not filled with zeros, and `{serverStatus: 1, <section>: 0}` leaves one
out on request. `mongostat` now shows its memory, cache and queue columns.

#### Changed

- WiredTiger runs with `statistics=(fast)`, mongod's setting, for both Rust
  servers.

#### Fixed

- Partly fixes RM-034 (`tests/backlog/test_rm_029_056.py::test_server_status_metrics`);
  `locks` is still absent. Pinned besides by
  `crates/secantus-storage-adapter/tests/server_status_wt.rs`.
