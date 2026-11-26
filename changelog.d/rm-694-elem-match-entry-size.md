### `$planCacheStats` sizes entries whose winner reads an `$elemMatch`

The Rust MongoDB server now reports `estimatedSizeBytes` for a plan cache
entry whose winning index scan reads an `$elemMatch` over operators, and for
one whose `$elemMatch` holds a `$not`, as mongod 8.2.11 does.

#### Fixed

- `estimatedSizeBytes` was left out of an entry whose winner read an
  `$elemMatch` (mongod tags the `ELEM_MATCH_VALUE` itself, not the
  predicates inside), and of any entry with a `$not` inside `$elemMatch`.
  The object form of `$elemMatch` remains open under RM-694.

Fixes RM-694 in part (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_not_inside_elem_match_entry_reports_its_size, tests/backlog/test_rm_found_2026_11_08_rm341.py::test_an_elem_match_the_winner_reads_reports_its_size)
