### Rust MongoDB server: `$bucket` parses, places and orders as mongod does

mongod rewrites `$bucket` into a `$group` keyed by a `$switch` over the
boundaries, followed by a `$sort` on `_id`, and checks the spec as it parses
it. The Rust server's `$bucket` was ported from the Python engine's. It
compared values the way Python does, which defers on any Decimal128. It
emitted buckets in boundary order with the default last, read a null
`default` as no default, and refused every malformed spec with 2 "not
supported". Measured against mongod 8.2.11 across 46 shapes, all of which
now match, error messages included.

#### Fixed

- Buckets come out sorted by `_id`, so a string `default` comes before date
  buckets and a numeric `default` below the boundaries comes first.
- Decimal128 boundaries and defaults, and mixed numeric boundaries, are
  placed by BSON comparison. They used to answer 2.
- An explicit null `default` is a default.
- Constant expressions in `boundaries` are evaluated. A field path is 40191.
- The spec's own errors, bare as mongod sends them:
  - `groupBy` not a path or expression is 40202;
  - `boundaries` not an array is 40200;
  - fewer than two boundaries is 40192;
  - boundaries of different types is 40193;
  - boundaries not strictly ascending is 40194;
  - a non-object `output` is 40196;
  - a `default` of the boundaries' type that falls inside them is 40199.

Fixes RM-253
(tests/backlog/test_rm_found_2026_10_21b.py::test_bucket_with_non_numeric_boundaries).
