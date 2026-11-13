### Explain counts the documents a sort with a limit kept

A `$sort` with a `$limit` behind it keeps only the documents the limit
asks for, and the Rust MongoDB server's `explain` now reports that. For
`[{$addFields: ...}, {$sort: {n: -1}}, {$limit: 200}]` over 300 documents,
the `$sort`'s `totalDataSizeSortedBytesEstimate` is the 200 documents its
top-k sort kept (131400 bytes), as on mongod 8.2.11, not all 300 (197100).
This also holds through a `$skip`, a stage that passes each document on, and
a second `$limit`, and the `$sort`'s `nReturned` now matches those cases too.

#### Fixed

- `secantus-commands` (`explain`): the run that gives a `$sort` its
  counters now carries the limit the whole pipeline pushes down to it
  (`limit_for_pushdown`, mongod's `extractLimitForPushdown`). Before, that
  run cut the pipeline off at the `$sort`, so it sorted everything.
  Fixes RM-516 (tests/backlog/test_rm_found_rm408.py::test_explain_counts_the_documents_a_sort_with_a_limit_kept),
  regression test tests/mongod_live/test_explain_memory_counters.py::test_a_sort_counts_the_documents_its_limit_keeps.
