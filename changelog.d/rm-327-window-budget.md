### `$setWindowFields` refuses at mongod's memory limit for rank functions and partitioned windows

On the Rust MongoDB server, a `$setWindowFields` stage under a lowered
`internalDocumentSourceSetWindowFieldsMaxMemoryBytes` refused at a budget a
few bytes, or a few hundred, off mongod's. A `$rank` over a hundred
documents ran under 375 bytes where mongod needs 1619: mongod's rank
functions read the sort key that `$sort` attaches to each document, so its
partition cache holds whole documents carrying that key, and the function
itself keeps its accumulator. A partitioned window ran 148 bytes short,
because mongod's partition iterator also counts the partition key and the
first document of the next partition, which it reads to find where the
current one ends. Both are now counted as mongod counts them, and the
refusal names what mongod names. Over a matrix of 48 window shapes, with and
without `partitionBy`, 40 now refuse at Linux mongod 8.2.12's limit to the
byte, up from 18. `$documentNumber` with no `sortBy`, and any rank function
over a two-field `sortBy`, is now refused with mongod's 5371602 instead of
answered.

#### Fixed

- `secantus-core` (`windowfields.rs`): a rank function caches whole
  documents with `$sort`'s sort key (`memlimit::SORT_KEY_METADATA`), reads
  only the document it answers, and counts its accumulator (376 / 368 / 368
  bytes, plus the last distinct key); a partitioned window counts the
  partition key and, once a window reads past the partition's end, the next
  partition's first document; a backward `$shift` holds the document leaving
  its endpoint. The 5414201 message reports the functions and the iterator's
  bytes.
- `$rank`, `$denseRank` and `$documentNumber` require a single-field
  `sortBy` (5371602).
- Fixes RM-327 (tests/backlog/test_rm_found_2026_10_26.py::test_window_budget_residuals)
- Fixes RM-405 (tests/backlog/test_rm_found_2026_11_08.py::test_rank_functions_need_one_sort_field)
