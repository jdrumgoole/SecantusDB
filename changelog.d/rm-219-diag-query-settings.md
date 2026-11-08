### The Rust MongoDB server answers getDiagnosticData and keeps query settings

`getDiagnosticData` now answers what mongod answers: the latest sample from its full-time diagnostic data capture. The server takes one every second, on the second, and keeps the most recent. On a replica-set member the sample holds `replSetGetStatus`, the `$collStats` storage statistics of `local.oplog.rs`, `config.transactions` and `config.image_collection`, and `serverStatus`. Each collector is stamped with its own `start` and `end`, and a failed collector keeps its error in the sample.

Query settings work as they do on mongod 8.2. `setQuerySettings` keys a setting by the query's `queryShapeHash`, given as a representative query or as the hash itself. A second set for the same shape merges into the first. `removeQuerySettings` removes it, and `$querySettings` lists them. The settings are kept where mongod keeps them, in `config.clusterParameters`, so they survive a restart, and `serverStatus.querySettings` counts them. A `find`, `distinct` or `aggregate` whose shape has a setting is refused with 411 `QueryRejectedBySettings` under `reject: true`. Under `indexHints` it is planned over the allowed indexes only: `$natural` allows the collection scan, in its direction, and the read's own `hint` and any index filter are ignored. `explain` reports the setting as `querySettings`. Every argument mongod refuses is refused here with mongod's code and message.

#### Added

- `getDiagnosticData`, with a collector thread in the server (`crates/secantus-commands/src/ftdc.rs`).
- `setQuerySettings`, `removeQuerySettings`, the `$querySettings` stage and `serverStatus.querySettings` (`crates/secantus-commands/src/query_settings.rs`).

#### Fixed

- Fixes RM-219 (tests/backlog/test_rm_found_2026_10_20b.py::test_commands_mongod_runs_are_commands_here), with regression tests in `tests/backlog/test_rm_219_diag_query_settings.py`: `test_get_diagnostic_data_samples_mongods_collectors`, `test_get_diagnostic_data_answers_the_latest_periodic_sample`, `test_set_query_settings_refuses_as_mongod`, `test_query_settings_merge_and_are_kept_as_mongods`, `test_query_settings_apply_to_the_reads_of_their_shape` and `test_query_settings_stage_refuses_as_mongod`.
- Filed: RM-416 (the `applyOps`, `dbCheck` and `filemd5` remainders carried over from RM-219), RM-417 (FTDC's transport collector, `diagnostic.data` files and parameters), RM-418 (`queryFramework` does not choose the engine; no `debugQueryShape`), RM-419 (`$collStats` refuses `local.oplog.rs`).
