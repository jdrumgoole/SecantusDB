### The Rust MongoDB server logs slow operations

A benchmark run on a cloud machine refused a row because one writer had not
finished an `insert_many` within ten seconds. Nothing could say whether the
server had stalled or the machine had: the server logged nothing about it, and
the harness had discarded the server's output and removed its store.

The Rust MongoDB server now writes a `Slow query` line for every operation
that runs for 100 ms or longer, as `mongod` does. Under heavy disk contention
on a Mac, both servers stall an insert for seconds at a time (`mongod` up to
4.1 s, the Rust server up to 3.3 s over two 60-second runs each); `mongod`'s
own line attributes its stall to waiting on the WiredTiger cache. The
ten-second stall itself was not reproduced.

#### Added

- `secantusd-rs` logs `Slow query` with the namespace, command, outcome and
  duration of any operation at or over `slowms` (default 100), at `INFO` and
  in the `getLog` buffer. Time an awaitData `getMore` spends waiting is not
  counted.
- The benchmark harness (`bench/concurrency.py`) keeps the server's log and
  store when a row fails, prints where they are, and shows the end of the log.
  It runs the Rust server at `INFO` so the slow lines are in it.

#### Fixed

- `profile`'s `slowms` and `sampleRate` are server-wide on the Rust MongoDB
  server, as on `mongod` 8.2.11. They were kept per database, so a threshold
  set through one database read back as 100 through another.
- The Rust server's documentation gave the `--cache-size` default as `1G`. It
  is `4G`.
