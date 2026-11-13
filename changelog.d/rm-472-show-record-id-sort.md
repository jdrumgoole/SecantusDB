### Rust MongoDB server: a sort that keeps its documents' metadata is a `default` sort

A read's blocking SORT now reports the type mongod 8.2.11 gives it. As
in mongod's `canUseSimpleSort`, a sort is `simple` only when the read
depends on no metadata except `sortKey` and does not keep the RecordId.
`showRecordId`, or a projection that reads `{$meta: "recordId"}`,
`"indexKey"`, `"randVal"`, `"searchScore"` or `"geoNearDistance"`, now
makes it `default`. Before, only a sort of index keys below the FETCH
was `default`. The plan cache's shape follows the same type
(`SORT_DEFAULT`). What the sort does, and the documents it returns, are
unchanged.

#### Fixed

- `find({z: 0}, {z: 1}).sort({z: 1}).showRecordId()` explains as
  `PROJECTION_DEFAULT <- SORT (default) <- COLLSCAN`, where it showed a
  `simple` sort. Fixes RM-472
  (tests/backlog/test_rm_found_2026_11_09_rm373.py::test_a_sort_beside_show_record_id_is_a_default_sort;
  also tests/backlog/test_rm_found_2026_11_09_rm373.py::test_a_sort_beside_a_metadata_projection_is_a_default_sort).

#### Known gaps

- Filed RM-577: a `default` sort of documents still counts the
  documents in `totalDataSizeSorted`, not its working-set members
  (402 where mongod counts 1122).
- Filed RM-578: a `$near` read over a `2d` index explains as a
  COLLSCAN, where mongod plans `GEO_NEAR_2D`.
