### $trim / $ltrim / $rtrim validate their input and chars arguments

The trim operators silently ignored a non-string `chars` argument (falling back to
whitespace trimming) and reported a non-string `input` with a generic error.
mongod validates both: a non-string `input` is `Location50699` and a non-string
`chars` is `Location50700` (each message names the offending value and type). A
null / missing `input` yields `null`, and — unlike the whitespace default — an
explicit `chars: null` also yields `null`. Both servers now match.

The Python server carries mongod's codes; the Rust core defers the non-string
cases (so the Rust server rejects them) and now returns `null` for a `chars: null`
rather than deferring. Three-way mongod 7.0.12-verified.

#### Fixed

- `$trim` / `$ltrim` / `$rtrim` reject a non-string `input` (`Location50699`) or
  `chars` (`Location50700`) instead of erroring generically or silently ignoring
  `chars`, and yield `null` for a `chars: null` (both servers).
