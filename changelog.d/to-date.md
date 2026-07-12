### `$toDate` conversion expression on both servers

The `$toDate` aggregation expression now works on the pure-Python and Rust
servers. `$toDate: <expr>` is the shorthand for `$convert: {input: <expr>, to:
"date"}`, and SecantusDB implements it as exactly that — a date is returned
unchanged, an int/long/double is read as milliseconds since the Unix epoch, and
an ISO-8601 string is parsed, while `null` or a missing field yields `null`.

Because `$toDate` delegates straight to the existing `$convert`-to-date path, it
inherits precisely the same supported inputs and errors: whatever `$convert` can
turn into a date, so can `$toDate`, with no separate conversion code to drift.
The Rust engine handles the date passthrough natively and defers the numeric /
string parses to the Python oracle, keeping the two engines byte-for-byte in
step (pinned by the expression parity harness).

#### Added

- `expressions.py` / `secantus-core`: `$toDate` aggregation expression operator,
  delegating to the existing `$convert`-to-date conversion.
