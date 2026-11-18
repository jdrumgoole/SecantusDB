### `serverStatus` reports the spill engine

The Rust MongoDB server's `serverStatus` now has mongod 8.2's
`spillWiredTiger` section: the bytes the spill engine's tables hold
(`storageSize`, 0 while nothing is spilled), its `uri` and `version`, and
the block-manager, cache and session statistics mongod reports at
`spillWiredTigerServerStatusVerbosity` 1, placed before `storageEngine`
as mongod places it. Eleven of mongod's counters are WiredTiger 12's and
are absent here, as the vendored WiredTiger does not keep them.

#### Fixed

- Fixes RM-614 (tests/backlog/test_rm_found_2026_11_13_rm325.py::test_server_status_reports_the_spill_engine)
