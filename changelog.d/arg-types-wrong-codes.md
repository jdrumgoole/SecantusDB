### Malformed pipeline stages report MongoDB's error code

A `$lookup`, `$group`, `$sort` or `$unwind` stage whose specification was the
wrong type returned an error — just not the one MongoDB returns. `$lookup`
answered TypeMismatch where MongoDB answers FailedToParse; `$group` and `$sort`
answered generic codes in place of their own; `$unwind` answered a message about
a path when the problem was that the specification was not a path at all. Codes
are what a driver branches on, so a client checking for a parse failure saw a
type error instead.

`find`'s `min` and `max` cursor bounds had the same shape: a wrong-typed bound
reached the index bound-checker and came back as an index error, where MongoDB
type-checks the argument first and never gets that far.

Two of these were one condition doing two jobs. `$sort` used a single test for
"not an object, or empty", so a spec of `5` was reported as "must have at least
one sort key" — true of `{}`, meaningless for a number. `$unwind` reported
"expected a string as the path" for a spec with no `path` key at all, and the
missing-`$` message for an empty one, where both are "no path specified".
Splitting them means an empty `$sort` and a missing `$unwind` path keep their
own distinct codes, which is why both are now pinned by tests.

Also fixed while separating those cases: `{$unwind: {other: 1}}` now reports the
unrecognized option rather than complaining about the path it also lacks.

With this the wrong-typed-argument sweep reports **87 of 87 cases clean**, from
24 crashes and 44 divergences when it started.
