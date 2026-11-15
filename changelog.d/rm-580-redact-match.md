### Rust MongoDB server: a `$match` after a `$redact` is copied ahead of it

`explain` and the run now copy the part of a `$match` behind a `$redact`
that the `$redact` cannot change ahead of it, as mongod 8.2.11 does: into
the query that reads the collection, and listed again just ahead of the
`$redact`.

#### Fixed

- `[{$redact: "$$KEEP"}, {$match: {z: 0}}]` read the whole collection and
  listed only the two stages written; it now reads `{z: {$eq: 0}}` and lists
  `$match`, `$redact`, `$match`, as mongod does. The part copied is mongod's
  `redactSafePortion`: comparisons and `$in` of values that are not arrays,
  documents or null, `$type`, `$regex`, `$mod`, `$elemMatch`, an `$and` of
  what is safe and an `$or` whose every branch is; never `$exists` or
  `$size`.

Fixes RM-580 (tests/backlog/test_rm_found_2026_11_12_rm360.py::test_a_match_after_a_redact_is_copied_ahead_of_it)
