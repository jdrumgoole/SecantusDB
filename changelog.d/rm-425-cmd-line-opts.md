### Rust MongoDB server: `getCmdLineOpts` reports the command line

`secantusd-rs` now answers `getCmdLineOpts` as mongod 8.2 does: `argv` is the
command line the process was started with, and `parsed` holds each option it
was given under its mongod configuration-file path (`--port 27031
--storage-path D` is `{net: {port: 27031}, storage: {dbPath: D}}`). The same
`parsed` is the `Options set by command line` startup log line and the
`cmdLine` of each `local.startup_log` record. Flags with no mongod option
(`--oplog-async`, `--cache-size`, ...) do not appear.

#### Fixed

- Fixes RM-425 (tests/backlog/test_rm_cmd_line_opts.py::test_get_cmd_line_opts_reports_the_command_line)
