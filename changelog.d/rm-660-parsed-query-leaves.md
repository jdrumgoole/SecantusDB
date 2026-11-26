### explain's `parsedQuery` parses an `$elemMatch` document and `$nin: []` as mongod does

`canonical_match` now tells an `$elemMatch` value form from its document form
by the first field, as mongod's `parseElemMatch` does, and drops a `$nin: []`
(or `$not: {$in: []}`) beside another clause.

#### Fixed

- `{t: {$elemMatch: {b: 1, $and: [{q: 1}], a: {$gt: 0}}}}` is explained as
  `{t: {$elemMatch: {$and: [b, q, a]}}}`, not echoed as written.
- `{c: 1, a: {$nin: []}}` is explained as `{c: {$eq: 1}}`; two such clauses
  alone are the empty query.
- A `$not` of several operators parses to the `$nor` of their `$and`; the
  Boolean simplifier still rewrites it beside a leaf (RM-778).

Fixes RM-660 (tests/backlog/test_rm_found_2026_11_14.py::test_parsed_query_parses_these_leaves_as_mongod)
