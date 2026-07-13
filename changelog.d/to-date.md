### `$toDate` conversion expression on both servers

The `$toDate` aggregation expression now works on the pure-Python and Rust
servers. `$toDate: <expr>` is the shorthand for `$convert: {input: <expr>, to:
"date"}`, and SecantusDB implements it as exactly that — a date is returned
unchanged, an int/long/double is read as milliseconds since the Unix epoch, and
an ISO-8601 string is parsed, while `null` or a missing field yields `null`.

Because `$toDate` delegates straight to the existing `$convert`-to-date path, it
inherits precisely the same supported inputs and errors: whatever `$convert` can
turn into a date, so can `$toDate`, with no separate conversion code to drift.
The Rust engine's `$convert`-to-date was also widened to convert an int / long /
double (epoch milliseconds) to a date natively, so both `$convert` and `$toDate`
now compute the numeric case on the Rust server rather than deferring; ISO-string
and ObjectId inputs still defer to the Python oracle (matching `$dateFromString`'s
partial Rust support). The two engines stay byte-for-byte in step (pinned by the
expression parity harness).

#### Added

- `expressions.py` / `secantus-core`: `$toDate` aggregation expression operator,
  delegating to the existing `$convert`-to-date conversion; the Rust
  `$convert`-to-date path gains native int/long/double → epoch-millis conversion.
