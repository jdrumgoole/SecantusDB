### A `distinct` of a view runs as mongod's aggregation, and counts what it examined

mongod 8.2 runs a `distinct` of a view as an aggregation over the view --
the query's `$match`, then `$replaceRoot` to every value at the key
(`$_internalFindAllValuesAtPath`), `$unwind` and a `$group` with
`$addToSet` -- and its `Slow query` line and profile entry count what that
aggregation's plan examined and say its cursor is exhausted. The Rust
MongoDB server read the view's documents another way and printed none of
`keysExamined`, `docsExamined` or `cursorExhausted`, and printed
`hasSortStage` for a view whose `$sort` the index answers. It now runs the
same aggregation, so the counts, the plan and the absent sort stage are
mongod's.

#### Added

- `$_internalFindAllValuesAtPath`, which mongod 8.2.11 accepts from a
  client too: every value at a dotted path, as a collection's `distinct`
  reads them, once each in BSON order under the collation, with mongod's
  parse errors (16020, 5511201, 9567004 and the `FieldPath` ones). It was
  refused as an unknown expression (31325).

#### Fixed

- A `distinct` of a view counts the keys and documents its plan examined
  and says its cursor is exhausted, in its `Slow query` line and its
  profile entry, as mongod 8.2.11's do.
  Fixes RM-493 (tests/backlog/test_rm_view_lines.py::test_a_distinct_of_a_view_counts_what_it_examined),
  with regression tests in `tests/mongod_live/test_distinct_of_a_view.py`
  for the values, refusals and counts of a `distinct` of a view and for
  the expression's parse.
- Filed while working it: RM-594 (a bare `"$"` is read as a missing field
  where mongod refuses it, 16872).
