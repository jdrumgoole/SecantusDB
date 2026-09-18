### `$toDouble` of a date was off by the server's time zone

The Python server converted a date to a double by reading it as the host's
local time, so on any server not running in UTC, `$toDouble` of a date came back
off by the host's UTC offset. `$toLong` and `$toDecimal` were already correct,
which is how it went unnoticed: every CI runner is UTC, and a London box is UTC
in winter.

That is now also something CI can see. Both Windows lanes run on a Newfoundland
host (never UTC, observes DST, a half-hour offset), and this bug is the first
thing they caught. The same change gives the Rust server a local-time rendering
test that runs on Windows, where the Rust engine's `TZ` bug (#1468) used to
hide.

#### Fixed

- `expressions.py`: `$toDouble` / `$convert` to double of a date pins UTC
  instead of reading the naive BSON date as local time.

#### Testing

- `.github/workflows/test.yml`: the `test-windows` and Windows
  `storage-engine` lanes set the host zone to Newfoundland and assert the change
  took.
- `tests/test_rust_server_timestamp_local_time.py`: drives the real Rust server
  through pymongo for `$toLower` of a `Timestamp`, with and without `TZ`, and
  runs in the `storage-engine` job.
