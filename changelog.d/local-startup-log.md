### Each start is recorded in `local.startup_log`

mongod keeps a capped `local.startup_log` and adds one document to it every time it starts, which is where a tool looks to find out when, and how, a server was started. The Rust MongoDB server had no such collection, so that lookup found nothing and `compact` of it was a NamespaceNotFound. It now writes the record as mongod does, on every start: the collection is created capped at 10 MB, and each document carries `_id` `<hostname>-<milliseconds>`, `hostname`, `startTime`, `startTimeLocal`, `cmdLine`, `pid` and `buildinfo`.

#### Fixed

- `secantus-commands` (`serverinfo`), `secantus-server`: `bind` records the start in `local.startup_log` before it serves. `startTime` is to the second, `startTimeLocal` is mongod's `ctime` string to the millisecond without the year, `cmdLine` is what `getCmdLineOpts` reports as `parsed`, and `buildinfo` is the `buildInfo` reply without its `ok`.
- Fixes RM-319 (tests/backlog/test_rm_192_compact.py::test_local_startup_log_records_each_start)
- Regression tests: `crates/secantusdb/tests/embedded.rs::each_start_is_recorded_in_local_startup_log`, `serverinfo::tests::startup_log_time_is_ctime_to_the_millisecond_without_the_year`.
- Filed RM-425: `getCmdLineOpts` does not report the command line the server was started with, and so neither does `cmdLine` (`tests/backlog/test_rm_cmd_line_opts.py::test_get_cmd_line_opts_reports_the_command_line`).
