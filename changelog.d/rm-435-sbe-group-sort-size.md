### Rust MongoDB server: a `$sort` after a `$group` run in SBE counts mongod's bytes

A `$sort` behind a `$group` that mongod 8.2 runs in SBE now reports the
`totalDataSizeSortedBytesEstimate` mongod does: SBE hands each group to the
classic `$sort` as a document built field by field, each sub-document its
own built storage, which costs more than the document the classic engine
builds.

#### Fixed

- `[{$group: {_id: "$a"}}, {$sort: {_id: 1}}]` over ten documents counted
  351 bytes where mongod 8.2.11 counts 735.
  Fixes RM-435 (tests/backlog/test_rm_found_2026_11_08_rm071.py::test_a_sort_after_a_pushed_down_group_sizes_what_sbe_hands_it)

#### Found

- RM-693: a `$group` with `$mod` in its `_id`, or an expression object in an
  accumulator, is not pushed into SBE.
