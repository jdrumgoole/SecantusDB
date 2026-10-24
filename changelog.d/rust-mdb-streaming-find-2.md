### Rust MongoDB server: more `find` cursors read on as `getMore` asks

A `find` cursor now streams over more of mongod's plans, so its `getMore`
sees documents inserted, changed or deleted since the last batch, as
mongod's does, and its `Slow query` lines and profile entries count each
batch's own keys and documents.

#### Fixed

- A backward index scan (a sort the index returns reversed), `$natural: -1`,
  an `_id` index scan (an `_id` sort or range), and a covered plan stream.
  A covered plan's lines report `docsExamined: 0`, as on mongod 8.2.11. A
  collection holding an `_id` that is a document, BinData, JavaScript or
  MaxKey is still read whole: this store's keys for those keep an older
  encoding whose order is not mongod's. Fixes RM-080 in part
  (`tests/backlog/test_rm_057_084.py::test_backward_index_scan_get_more_sees_writes_made_since_the_find`,
  `::test_id_index_get_more_sees_writes_made_since_the_find`,
  `::test_id_sorted_get_more_sees_writes_made_since_the_find`,
  `::test_natural_backward_get_more_sees_writes_made_since_the_find`,
  `::test_id_sort_over_document_ids_is_in_mongods_order`) and RM-023 in part
  (`tests/backlog/test_rm_001_028.py::test_slow_query_lines_count_each_batch_of_backward_and_id_scans`).
- A covered `find` with a `limit` streams too: the planner puts the LIMIT
  above the covered projection.

#### Changed

- The storage reads behind streaming cursors (`records_after`,
  `index_entries_from`) take a direction, and `index_entries_from` reads
  the `_id` index. Both Rust servers link `secantus-storage`; the
  PostgreSQL server does not call these.
