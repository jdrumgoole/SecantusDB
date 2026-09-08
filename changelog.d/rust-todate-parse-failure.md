### `$toDate` rejected every timestamp carrying milliseconds

Chasing the error *message* for an unparseable date string turned up a much
larger bug behind it: `parse_iso` required exactly 19 characters before a `Z`, so
**every ISO timestamp with a fractional second failed** — `2020-01-01T00:00:00.123Z`,
the ordinary form for a BSON date. It came back as an error on the Rust server
where mongod parses it.

Measured against mongod 8.2.11 and now reproduced: a fractional second takes 1..n
digits and is **truncated to milliseconds** (`.1` is 100 ms, `.1234567` is 123),
with or without a `Z` or a `±HH:MM` offset. `YYYY-MM` is the first of that month;
a bare `YYYY` is not a date at all.

#### The error surface, too

A failed parse used to return `Conv::Failed`, which on this server surfaces as
`2 BadValue: aggregation pipeline uses a stage or operator not supported by the
Rust server` — **false**, since `$toDate` is supported and the string was at
fault, and a different code from mongod's `241 ConversionFailure`. It now carries
241 always, with mongod's exact text for the two reproducible shapes: an empty
string names a literal NUL, everything else gets the incomplete-string message.

**Whitespace-only is not empty** for mongod — `''` is "Empty string" but `'  '`
is the incomplete message — which the Python server had backwards because it
tested the *stripped* text. Fixed on both servers.

#### Still not reproduced, and now shared rather than divergent

mongod's per-position timelib diagnostic (`'abc'` names the offending character
and where its scanner stopped) needs timelib's own lexer, its timezone
abbreviation tables and its per-position error accumulation. Both servers give
the same message instead, so they agree with each other while the gap stays
documented.

#### Measured

34 strings: **18 exact, 16 message-only, 0 with a wrong value or code** — from 0
exact with all 25 failure cases carrying the wrong code. Rust and Python agree on
23 of 23 failure strings.
