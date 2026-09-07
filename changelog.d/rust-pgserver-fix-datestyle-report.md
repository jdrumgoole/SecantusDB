### Rust pgserver: only report the TimeZone GUC, not DateStyle

The timestamptz-columns change began emitting a `ParameterStatus` for several
GUC_REPORT variables on `SET`, including `DateStyle`. But this server always
renders dates in ISO regardless of `DateStyle`, so reporting a `DateStyle`
change made the client (psycopg) switch its date parser to a style our output
never uses — mis-parsing every datetime. Reporting is now limited to
`TimeZone`, the one GUC whose change the output actually honours (a timestamptz
renders in it).

#### Fixed
- `SET datestyle` no longer breaks datetime parsing: the server reports only
  `TimeZone` via `ParameterStatus`, not GUCs it does not honour in output.
