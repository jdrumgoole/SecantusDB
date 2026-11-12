### Rust MongoDB server: a sort beside a multikey field sorts the index keys

A blocking sort on fields an index holds now runs on the index keys,
below the FETCH, whenever none of the sort's own paths is multikey, as
mongod 8.2.11 plans it. Before, any multikey path anywhere in the index
moved the sort above the FETCH, onto the fetched documents.

#### Fixed

- `find({y: {$gte: 0}}).sort({x: 1})` over `{y: 1, x: 1}`, with `y`
  multikey and `x` not, plans `FETCH <- SORT <- IXSCAN` with a
  `type: "default"` SORT that counts 780 bytes for twelve members, in
  `explain` and in the profile entry's `execStats`, where it planned
  `SORT <- FETCH <- IXSCAN` (`simple`, 900). The planner asks each sort
  path whether it is multikey, as `IndexScanNode::computeProperties`
  does, so a sort that names a multikey path (or a dotted path under an
  array) still sorts the fetched documents.

Fixes RM-477 (tests/backlog/test_rm_found_rm375.py::test_a_sort_beside_a_multikey_field_sorts_index_keys)

#### Found

- RM-559: a SORT_MERGE beside a null scan (`$in` with `null` under a
  sort) returns two documents whose sort keys tie in another order than
  mongod's
  (tests/backlog/test_rm_found_rm477.py::test_a_sort_merge_beside_a_null_scan_breaks_ties_as_mongod).
