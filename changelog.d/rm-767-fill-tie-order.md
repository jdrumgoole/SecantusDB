### A `$fill` passes its documents on in mongod's order

A `$fill` of values only now keeps every document where it was, as
mongod's `$addFields` does, and a `$fill` with a `method` sorts as the
`$setWindowFields` mongod parses it into: that sort overrides a `$sort` in
front of it, so documents whose `sortBy` keys tie come in the order they
had before that `$sort` (measured against mongod 8.2.11).

#### Fixed

- A values-only `$fill` reordered its input by `partitionBy` and `sortBy`;
  a `$fill` with a `method` behind a `$sort` kept that `$sort`'s order
  among ties.

Fixes RM-767 (tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_fill_passes_ties_on_in_mongods_order)
