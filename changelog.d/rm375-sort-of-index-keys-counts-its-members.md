### A sort of index keys counts what it sorted, and keeps to its budget

When the Rust MongoDB server sorts the index keys a scan found before it
fetches any documents (a SORT of `type: "default"`, below the FETCH), it now
counts that sort the way mongod 8.2 does. Each key it sorts counts its sort
key, the index key it was found at, and 32 bytes. The total shows up as the
stage's `totalDataSizeSorted` in a profile entry's `execStats`, and in
`serverStatus`'s `metrics.query.sort.totalBytesSorted` and `totalKeysSorted`.
The sort is also held to `internalQueryMaxBlockingSortMemoryUsageBytes`: past
it, a query without `allowDiskUse` is refused with error 292 at the same
budget mongod refuses it.

#### Fixed

- A SORT of index keys reported no `totalDataSizeSorted` in a profile entry's
  `execStats` (mongod: 780 for the twelve `{y, x}` keys of
  `find({y: {$gte: 0}}).sort({x: 1})`). It added nothing to
  `metrics.query.sort`, and no memory budget applied to it. The count now
  matches mongod 8.2.11 across key types, compound sorts, missing fields,
  dotted and multikey paths, a query collation, and a sort's own limit. It
  is refused past its budget less one file iterator, as mongod refuses it
  (916 bytes for those twelve keys). Fixes RM-375
  (tests/backlog/test_rm_found_2026_11_07.py::test_a_sort_of_index_keys_counts_the_data_it_sorted,
  tests/backlog/test_rm_found_rm375.py::test_a_sort_of_index_keys_counts_each_member_as_mongod_does,
  tests/backlog/test_rm_found_rm375.py::test_a_sort_of_index_keys_is_held_to_its_budget).
- Filed while working it: such a sort past its budget with `allowDiskUse`
  does not spill (RM-475); `explain` sizes it as documents (RM-476); and a
  sort beside a multikey index field sorts documents instead of keys
  (RM-477).
