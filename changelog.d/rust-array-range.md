### Rust server compares array-vs-array range bounds lexicographically

A range query whose bound is an array — `{a: {$gt: [1, 2]}}` — now evaluates on
the Rust server instead of erroring. The Rust matcher previously deferred any
array operand to a `Fallback`, which the Rust server surfaced as a `BadValue`;
it now compares the two arrays **whole-array lexicographically**, exactly as the
Python server (via Python's native `list < list`) and real `mongod` do.

The comparison recurses element-by-element: the first decisive element pair wins,
equal leading elements continue to the next pair, and if one array is a prefix of
the other the shorter one sorts first. A cross-type element pair (where Python's
`<` would raise `TypeError`) yields a clean no-match rather than an error, and an
array field compared against a *scalar* bound still rides the multikey element
path (`{a: [1, 3]}` matches `{a: {$gt: 2}}` because `3 > 2`). Only the exotic BSON
types (JS code / symbol / dbpointer / undefined) as a range operand still defer to
the Python engine.

Verified against real `mongod` 6.0 and pinned to the Python oracle by new curated
parity cases and Rust unit tests.

#### Fixed

- Rust server: `$gt` / `$gte` / `$lt` / `$lte` with an **array bound** (e.g.
  `{a: {$gt: [1, 2]}}`) now compares whole-array lexicographically instead of
  returning `BadValue`, matching the Python server and `mongod`. Array-vs-scalar
  bounds continue to match via the multikey element path; a cross-type element
  pair no-matches cleanly.
