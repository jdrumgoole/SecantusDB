### The SQL server reports IntervalStyle at startup

`IntervalStyle` is one of postgres's `GUC_REPORT` parameters — a real
server announces it in the startup `ParameterStatus` set so clients that
decode intervals themselves know which style to parse. SecantusDB's SQL
server tracked the setting internally, defaulting to `postgres`, but
never announced it.

That gap is invisible to psycopg's binary backend, because libpq keeps
its own copy of the value, which is why the pinned test configuration
never noticed. psycopg's pure-Python backend trusts the server instead:
with the parameter absent it sees a style of `unknown` and raises
`NotImplementedError` on any query returning an `interval`, so a client
configuration that works against real postgres failed against
SecantusDB. Anywhere psycopg falls back to the pure-Python
implementation — a platform with no binary wheel, or an explicit
`psycopg` install without the `[binary]` extra — hit it.

The server now sends `IntervalStyle`, and the guarantee is pinned at the
wire level rather than through a client, so it holds regardless of which
psycopg implementation is installed.

#### Fixed

- The SQL server reports `IntervalStyle` in its startup
  `ParameterStatus`, as real postgres does. Without it, psycopg's
  pure-Python backend could not decode an `interval` value and raised
  `NotImplementedError`.
