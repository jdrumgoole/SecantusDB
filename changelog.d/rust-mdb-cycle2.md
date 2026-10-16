### Rust MongoDB server: validator collation, `2dsphere` levels, and a refused index spec

Differences from mongod 8.2.11, each pinned by a test that compares the
two.

#### Fixed

- **A collection validator compares under the collection's collation.**
  An update that named a collation of its own (`simple`, `fr`) had its
  validator checked under that one. With a collection collation of `en`
  strength 2, `{k: "A"}` refused `k: "a"` with 121 where mongod accepts
  it.
- **`coarsestIndexedLevel` / `finestIndexedLevel` are accepted on a
  `2dsphere` index**, listed after `2dsphereIndexVersion`, and checked as
  mongod checks them: 16747, 16748, 16749 (the default coarsest level is
  2), 14 for a non-number and 2 for a fraction. They were refused (197).
- **`rolesInfo` lists roles in the order they were created**, as mongod
  does; a role dropped and created again goes last. The order is kept in a
  side table only the Rust server writes (as decided); a role it has no
  record of (one created by an older build or the Python server) keeps name
  order, ahead of the rest. It listed them by name.
- **A DBPointer operand to `$min` / `$max`** is compared by mongod's
  type order against any other type, so `{$min: {a: DBPointer}}` over
  `a: 5` changes nothing, as on mongod; it was refused as "not supported".
  Two DBPointers still defer (RM-127 stays open for that).
- **An exact duplicate `_id` in a collection with a default collation**
  is reported as mongod reports it, from the collated `_id`: the same
  message, `keyValue`, `hexEncoded` and `collation` as a second spelling.
  It was reported from the plain `_id` index.
- **A time-series measurement inserted with no `_id` is stored without
  one**, as on mongod. It was given an ObjectId.
- **A `createIndexes` refused for a spec with no `name` creates
  nothing.** The collection it would have created was left behind.

Fixes RM-096 (tests/backlog/test_rm_085_112.py::test_validator_reads_the_collections_collation),
RM-149 (tests/backlog/test_rm_geo_residuals.py::test_2dsphere_cell_levels_accepted_and_listed,
::test_2dsphere_cell_levels_are_checked), RM-150
(::test_refused_spec_leaves_no_created_collection), RM-127 in part
(tests/backlog/test_rm_113_138.py::test_dbpointer_min_update_operand), RM-142
(tests/backlog/test_rm_found_2026_10_14.py::test_exact_duplicate_id_under_a_default_collation_names_the_collation)
RM-089 in part
(tests/backlog/test_rm_085_112.py::test_measurement_without_id_is_stored_without_one)
and RM-029
(tests/backlog/test_rm_029_056.py::test_roles_info_lists_roles_in_creation_order,
::test_a_recreated_role_goes_last_and_an_update_keeps_its_place).
