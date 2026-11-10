### The Rust MongoDB server sizes a changed or built document as mongod stores it

mongod does not count a document a pipeline stage changed as the BSON it
would encode to. It keeps the BSON it read and, beside it, a cache of the
fields a stage looked at or wrote, in a buffer that grows in powers of
two; a document the engine builds from nothing, such as an expression
object's, is all cache. Its memory accounting counts that storage, and
the sort a `$setWindowFields` runs behind also attaches each document's
sort key. The Rust MongoDB server now models that storage, so the bytes
`explain` reports for windows, sorts and groups after an `$unwind`, an
`$addFields` or a computed partition match mongod 8.2, and the memory
budgets those stages spill or refuse against count the same bytes.

#### Fixed

- A new `docstore` module in `secantus-core` models mongod 8.2's
  `DocumentStorage` (`document_internal.h`): the BSON a document was read
  as, the fields read into its cache (`constructInCache`, which a sort's
  `memUsageForSorter` leaves out of the BSON it counts), the fields a
  stage wrote, the packed 26-byte `ValueElement`s and the power-of-two
  buffer with its hash table, the reserved buffer of a `$group`'s output
  (`MutableDocument(n)`), and the metadata a sort key adds. It reproduces
  the measured `$group` output table and the +224 / +240 sort overheads
  that were fitted before.
- A pipeline carries each document's storage through `$unwind`,
  `$addFields` / `$set`, `$unset`, `$match`, `$limit`, `$skip` and `$sort`
  to the `$sort`, `$setWindowFields` and `$_internalSetWindowFields` that
  count it, and a `$group`'s output to the stages after it.
- `$setWindowFields`: a computed `partitionBy` reads its fields and adds
  its key before the sort, and the window's own sort attaches the sort key
  as metadata, which both the sort and the partition cache count. A sort
  the query layer runs (one that moves ahead of `$unwind`s and `$match`es
  to the front) attaches none.
- An expression object's value (`{x: "$n"}`) counts the document it built,
  in a `$push`, an `$addToSet` and a `$group` `_id`; a classic `$group`
  counts a compound `_id`'s key as the array of its values, and returns
  the `_id` as `expandId` builds it.
- A leading `$unset` counts as read: mongod's scan takes it.
- Fixes RM-408 (tests/backlog/test_rm_found_2026_11_08_explain.py::test_changed_documents_are_sized_as_mongod_sizes_them).
- Fixes RM-396 (tests/backlog/test_rm_found_2026_10_26.py::test_added_fields_sort_spill_storage_size):
  a `$sort` after `$addFields` now spills at mongod's record boundaries,
  since the field the `$addFields` reads is counted in the cache and left
  out of the BSON the sorter counts.
- Filed RM-515 (a sub-document a stage reads or writes into), RM-516
  (`explain` counts every document a `$sort` with a `$limit` read), RM-517
  (`explain` lists a `$sort` a window's own sort replaces), RM-518
  (`$_internalInhibitOptimization` refused) and RM-519 (a document an
  inclusion `$project` built), each with a gap test in
  `tests/backlog/test_rm_found_rm408.py`.
