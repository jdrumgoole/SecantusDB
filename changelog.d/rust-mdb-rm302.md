### Rust MongoDB server: `getLog` answers with mongod's structured log

mongod's `getLog` lines are its structured log: one JSON document per line,
`{"t":{"$date":...},"s":"I",  "c":"COMMAND",  "id":51803,   "ctx":"conn8","msg":"Slow query","attr":{...}}`.
The Rust server's were text, `<date> I COMMAND Slow query {...}`, with no
`id` or `ctx` and the attributes after the message, so a tool that reads the
log as mongod writes it found nothing in them.

#### Fixed

- `secantus-commands` (`logbuf.rs`, `serverinfo.rs`), `secantus-core`
  (`logtime.rs`): every line is a structured entry, rendered as mongod's
  JSON formatter writes it (all measured against 8.2.11, 2026-10-24):
  - the columns are padded: `"s"` to 10, `"c"` to 16, `"id"` to 14;
  - the time is local, to the millisecond, with its offset;
  - attributes are relaxed Extended JSON with UUIDs as `{"$uuid": ...}`.
- `getLog` returns the latest 1023 lines, as mongod does.
  `totalLinesWritten` counts every line ever written, not just those kept.
  `startupWarnings` is the lines tagged for it, not every warning.
- `secantus-server`, `secantus-commands` (`slowop.rs`, `profiler/record.rs`):
  the lines carry mongod's ids, threads and attributes.
  - `MongoDB starting` (4615611, `initandlisten`: `pid`, `port`, `dbPath`,
    `architecture`, `host`) and `Waiting for connections` (23016,
    `listener`).
  - `Connection accepted` (22943, `listener`) and `Connection ended`
    (22944, the connection's thread), with `remote`, `isLoadBalanced`, a
    `uuid`, `connectionId` and `connectionCount`. The end was not logged
    before.
  - `Slow query` is 51803 on the connection's thread; the index build's line
    is on `IndexBuildsCoordinatorMongod-0`.
  - A profile entry that cannot be written is mongod's warning 20703.
  - A profile filter that throws writes no line, as on mongod.
- The server's standard error carries the same JSON lines.
- `ServerConfig` gains `db_path`, set by `secantusd-rs`, `secantus_mdb::Server`
  and the embedded handle, for `MongoDB starting`.
- Fixes RM-302 (`tests/backlog/test_rm_001_028.py::test_get_log_lines_are_structured_json`).

#### Filed

- RM-305: the lines mongod writes that this server does not. These are
  `client metadata` on a connection's first `hello`, `Received first command
  ...`, and mongod's startup reports.
