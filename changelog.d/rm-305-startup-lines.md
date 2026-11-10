### The log starts with the build, the system and the options, as mongod's does

The Rust MongoDB server's log now opens the way mongod 8.2 opens its own.
After `MongoDB starting` come `Build Info`, naming the version, the
allocator and the architecture the server was built for; `Operating
System`, naming the system it runs on; and `Options set by command line`,
holding the same options `getCmdLineOpts` reports. Each carries mongod's id
on mongod's `initandlisten` thread, so a `getLog` check or a log reader that
looks for mongod's first lines finds them here too.

#### Fixed

- Startup writes mongod's `Build Info` (CONTROL 23403), `Operating System`
  (51765) and `Options set by command line` (21951) lines after `MongoDB
  starting`, with mongod's attributes, taken from `buildInfo`, `hostInfo`
  and `getCmdLineOpts` as mongod takes them.
  Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_writes_build_os_and_options_lines);
  the storage and startup-complete lines stay open under RM-305, and
  `buildInfo`'s architecture name (`aarch64` where mongod on macOS says
  `arm64`) is filed as RM-500.
