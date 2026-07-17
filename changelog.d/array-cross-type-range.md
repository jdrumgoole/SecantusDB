### Range operators order array elements by full BSON type order, like mongod

A third comparison bug from the driver-gauge triage: `$gt` / `$lt` against an
array bound compared elements pairwise, but a *cross-type* element pair made
both servers return no match — `{a: {$gt: [1, 2]}}` skipped `{a: [1, "x"]}`
even though mongod matches it (a string element outranks a number element in
BSON order, so `[1, "x"] > [1, 2]`). Python's native list comparison raises
`TypeError` on `"x" > 2` (swallowed to a no-match) and the Rust matcher
returned no-match on any incomparable element pair. Both now order array
elements by full BSON order (type rank first) via the shared `_bson_lt`
comparator, verified three-way against a live mongod 7.0.12 probe.

#### Fixed

- Range operators order two arrays element-by-element in full BSON order, so a
  cross-type element pair still orders instead of silently no-matching (both
  servers).
