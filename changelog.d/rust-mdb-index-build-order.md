### The Rust MongoDB server keeps its indexes in the order they were built

mongod lists a collection's indexes in the order they were built, checks
them for a duplicate key in that order, and tries their plans in it. The
Rust MongoDB server kept no build order and used name order for all three.
So `listIndexes` after `create_index("z")` then `create_index("a")`
answered `a_1, z_1`, and a document duplicating both unique indexes was
refused by `a_1` where mongod names `z_1`.

#### Fixed

- **Each index records its build order** in a catalog option, `buildSeq`,
  stripped from `listIndexes` like the other catalog bookkeeping.
  `listIndexes`, the duplicate-key check and the planner's catalog all
  follow it, with `_id_` first. A recreated index goes to the end, as on
  mongod. A store written before this keeps its indexes in name order,
  ahead of any built later.

Fixes RM-081, which also covered RM-107
(tests/backlog/test_rm_057_084.py::test_list_indexes_in_build_order,
::test_duplicate_key_names_the_first_built_unique_index,
tests/backlog/test_rm_085_112.py::test_list_indexes_in_creation_order).

### The Rust MongoDB server's wire protocol, collation and pipeline stages, closer to mongod

More differences from mongod 8.2.11, each pinned by a test that compares
the two.

#### Fixed

- **An opcode the server does not serve no longer closes the connection.**
  mongod sends nothing back and answers the next request on the same
  socket, and so does this server now. A body too short to read, and the
  removed `OP_INSERT` / `OP_UPDATE` / `OP_DELETE` / `OP_KILL_CURSORS`,
  still close it, as on mongod.
- **A legacy `OP_GET_MORE` is refused with mongod's `OP_REPLY`** (flags 2,
  `$err` with code 5739101), and the connection is kept.
- **An exhaust `getMore` stream ends on the reply that closes the cursor.**
  The short last batch now goes out with cursor id 0 and no `moreToCome`.
  Each frame's `responseTo` names the previous frame, as on mongod. It used
  to send the last batch under the live id, then one empty frame more.
- **`serverStatus` reports `network.compression`**: bytes in and out for
  each of snappy, zstd and zlib, counted as mongod counts them.
- **`Did you mean` for a three-letter locale code** is ICU's: `eng` is
  read as `en` and `ENG_usa` as `en_US`. The tables come from ICU (182
  languages, 257 regions); 666 spellings now match.
- **A pipeline that involves a view with another collation** (`$lookup`,
  `$graphLookup`, `$unionWith`, or an `$out` / `$merge` target) is
  mongod's 167 `Cannot override a view's default collation`, checked
  before anything runs.
- **`$unionWith` with no `coll`** runs when its pipeline starts with
  `$documents`; it was refused with 9.
- **`$lookup` with `from: {db, coll}`** is mongod's own refusal: 9 naming
  both parts, or 14 for a part that is not a string. It was 2.

Fixes RM-144, RM-145, RM-146 and RM-147
(tests/backlog/test_rm_found_2026_10_15.py), RM-096 in part
(tests/backlog/test_rm_085_112.py::test_did_you_mean_for_a_three_letter_code,
::test_lookup_from_a_view_with_another_collation), RM-179
(::test_union_with_documents_needs_no_collection) and RM-180
(::test_lookup_from_a_db_and_coll_document).
