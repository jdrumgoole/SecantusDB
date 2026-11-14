### Rust MongoDB server: a count served by an index is explained as mongod's slot-based COUNT_SCAN

A `$count`, or a `$group` that reads no field, over an index scan that needs
no fetch, and `count` on a view so served, is now explained as mongod 8.2.11
explains it: `explainVersion: "2"`, a `queryPlan` of `GROUP` over
`COUNT_SCAN`, and a slot-based plan whose `ixseek` between two KeyStrings is
run over the index's keys, so `keysExamined`, `seeks` and `numReads` come
from executing it.

#### Fixed

- `explain` of a count over a forward scan of a single-field index that is
  not multikey, sparse, partial or collated (an equality to null included,
  as `IS_COUNT` plans it) no longer reports the classic `FETCH` / `IXSCAN`
  cursor. Fixes RM-072 (tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view_over_an_index;
  regression tests in tests/mongod_live/test_sbe_count_scan_explain.py).
  A view count whose filter a `FETCH` finishes is filed as RM-695.
