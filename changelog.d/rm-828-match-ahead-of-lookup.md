### A `$match` moves ahead of a `$lookup` it does not read

The Rust MongoDB server's explain now moves a `$match` (or the part of it)
that does not read a `$lookup`'s `as` field ahead of the `$lookup`, into the
query, as mongod 8.2.11 does.

#### Fixed

- `explain` of `[{$lookup: ...}, {$match: {a: 1}}]` lists the `$match` in the
  cursor's `parsedQuery`, not after the `$lookup`.
  Fixes RM-828 (tests/backlog/test_rm_lookup_sbe.py::test_a_match_moves_ahead_of_a_classic_lookup_that_does_not_read_it)
