### Rust MongoDB server: `sortKey` and `$geoNear` metadata in aggregate, missing values in `$group`, DBPointer updates

#### Fixed

- **`{$meta: "sortKey"}`, `{$meta: "geoNearDistance"}` and
  `{$meta: "geoNearPoint"}` in an aggregate pipeline** give mongod's
  values.
  - **After a `$sort`:** the sort key is an array with one value per sort
    field, holding the value the sort compared.
  - **After a `$geoNear`:** the distance and the point, and a sort key of
    `[distance]`.
  - **Lifetime:** both pass through `$match`, `$limit`, `$skip`,
    `$project`, `$addFields`, `$unwind` and a later `$sort`, and into a
    `$group`, as on mongod 8.2.11. With no `$sort` in reach, `$addFields`
    gives `[]`.
  - **Before:** `sortKey` was refused as unsupported, and the distance
    and point were refused with 40218 even after a `$geoNear`.

- **A missing value in `$group`** is what mongod makes it.
  - **Keys:** a missing key is null, and so is the missing field of a
    one-field document key (`{a: "$nope"}` groups under `{a: null}`). A
    document key of several fields drops a missing field.
  - **Accumulators:** `$push` / `$addToSet` skip an expression that
    evaluates to missing (`$ifNull` of two missing fields, a `$meta` with
    nothing to give); `$first` / `$last` give null for it.
  - **Before:** a `$group` keyed on `{$meta: "recordId"}` was refused,
    `{a: "$nope"}` grouped under `{}`, and `$push` wrote a null for each
    missing value.

- **A document holding a DBPointer can be updated.** Every update that
  changed such a document was refused, a `$set` of another field included,
  because change detection asked equality about the DBPointer. Two
  DBPointers now compare as mongod compares them: the size of the value
  first (a shorter namespace is smaller), then the namespace, then the id.

Fixes RM-083 (tests/backlog/test_rm_057_084.py::test_meta_sort_key_in_aggregate,
tests/backlog/test_rm_057_084.py::test_group_keyed_on_meta,
tests/backlog/test_rm_057_084.py::test_meta_geo_near_distance_after_geo_near,
tests/backlog/test_rm_found_2026_10_17.py::test_sort_key_and_geo_near_metadata_through_stages),
RM-184 (tests/backlog/test_rm_found_2026_10_17.py::test_group_key_with_a_missing_value,
tests/backlog/test_rm_found_2026_10_17.py::test_accumulating_a_missing_value),
RM-127 (tests/backlog/test_rm_113_138.py::test_dbpointer_min_against_a_dbpointer,
tests/backlog/test_rm_found_2026_10_17.py::test_updating_a_document_that_holds_a_dbpointer).
