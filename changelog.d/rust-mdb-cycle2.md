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
- **`distinct` over a multikey or collated index answers as mongod's
  DISTINCT_SCAN does**: from the index keys, each array unwound once, a
  missing field as `null`, `[]` as `undefined`; through a collated index,
  from each key's first document. It read the documents, so an array-valued
  field came back whole and a collated index kept a `null` mongod leaves
  out. A query over a multikey key field no longer plans a DISTINCT_SCAN.
- **`$toHashedIndexKey`** hashes as mongod's hashed index does (MD5 of the
  canonical type and value, numbers squashed to a 64-bit integer), checked
  bit for bit on 137 values. It was refused.
- **`$densify` with partitions or explicit bounds** runs mongod's own
  `$_internalDensify`: the documents sorted by partition and field, an
  off-grid value in its sorted place, and the tails after them.
- **A regular expression may be up to 32,764 bytes**, as on mongod. A
  longer one is 51091 in a query and 51111 in `$regexMatch` and its
  siblings, which also report other invalid patterns with 51111 now. The
  cap was 1,000 characters. A long pattern with no metacharacters is a
  plain substring search.
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
RM-171 and RM-172 (tests/backlog/test_rm_found_2026_10_16.py::test_distinct_over_a_multikey_index,
::test_distinct_through_a_collated_index_leaves_out_null), RM-124
(tests/backlog/test_rm_113_138.py::test_to_hashed_index_key), RM-157
(tests/backlog/test_rm_085_112.py::test_densify_partition_bounds_emit_in_partition_order),
RM-120 (tests/backlog/test_rm_113_138.py::test_regex_over_1000_characters)
and RM-029
(tests/backlog/test_rm_029_056.py::test_roles_info_lists_roles_in_creation_order,
::test_a_recreated_role_goes_last_and_an_update_keeps_its_place).
