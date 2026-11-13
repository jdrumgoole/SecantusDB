### Rust MongoDB server: a `$lookup` pipeline's `$sort` sets the sort key

A `$sort` inside a `$lookup` pipeline now sets the sort key that pipeline's
own `$_internalSetWindowFields` ranks and `{$meta: "sortKey"}` read, as
mongod 8.2.11 does. Before, the sub-pipeline saw no key, so every joined
document tied at rank 1.

#### Fixed

- The `$lookup` sub-pipeline is rewritten for sort metadata once, as the
  top-level pipeline is, and then run for every outer document.
  Fixes RM-521 (tests/backlog/test_rm_found_2026_11_10_rm410.py::test_a_lookup_sub_pipeline_sort_sets_the_key)
- Filed while working it: RM-624 (the same gap in `$unionWith`), RM-625
  (`{$meta: "sortKey"}` in a `$facet` is refused rather than `[]`) and
  RM-626 (a rank behind `$documents` and a `$sort` is answered where mongod
  refuses it).
