### `returnKey` explains a SORT_KEY_GENERATOR over an index's sort

A `returnKey` find whose projection asks for `{$meta: "sortKey"}`, sorted
by an index, now explains as mongod 8.2.11 does: `RETURN_KEY <-
SORT_KEY_GENERATOR <- IXSCAN`.

#### Fixed

- The plan had no SORT_KEY_GENERATOR under RETURN_KEY.
  Fixes RM-706 (tests/backlog/test_rm_found_rm568b.py::test_return_key_with_a_sort_key_over_an_index_sort_explains_a_key_generator)
