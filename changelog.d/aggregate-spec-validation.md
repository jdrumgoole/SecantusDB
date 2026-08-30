### Malformed aggregation stages report MongoDB's error, not a generic one

A malformed `$sample`, `$unwind`, `$bucket`, `$densify` or `$fill` spec reported
`BadValue` on the Rust server, whatever was actually wrong with it. The engine
signals "cannot do this" without a code, so a *typo* was indistinguishable from
an unimplemented feature — a driver matching on the code saw the wrong error,
and a caller could not tell which of the two had happened.

All five now report MongoDB's own code and message: a negative `$sample` size is
`28747`, an unprefixed `$unwind` path is `28818`, a one-element `$bucket`
boundary list is `40192`, a zero or negative `$densify` step is `5733401`, and an
unknown `$fill` method is `6050202`.

Validation happens before the pipeline runs, which is where `$facet` already
validated its own spec, so the engine's error type did not have to widen.

Two of these were wrong on the Python server too, in a way that only a
message-level comparison shows: `$sample` said "must not be negative" where
MongoDB says "must be a positive integer", and `$bucket` reported "found 1."
instead of "found 1 value(s)." The codes had always matched, which is why an
earlier comparison of codes alone reported them as correct.
