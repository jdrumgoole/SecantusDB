### `$currentOp` shows the op a running `bulkWrite` is on

A long `bulkWrite` used to appear in `$currentOp` as one opaque command on `admin.$cmd`, so a filter on the collection it was writing to never found it, and nothing said which of its ops was running. The Rust MongoDB server now lists it the way mongod does: op `bulkWrite`, under the namespace of the op it is running, with that op as its `command` and its running time counted from the op's start. The listing moves on as each op starts. Once the ops finish, the command's own entry comes back.

#### Fixed

- `secantus-commands` (`ops`, `bulkwrite`): a running `bulkWrite` op is listed as mongod 8.2 lists it. That means the op's fields in declaration order, with `multi` / `upsert` defaults, a named hint as `{$hint: name}` and an empty one left out, then the command's `comment`. A group of consecutive inserts is listed as `{insert: <nsInfo index>, documents: [...]}`. The opid stays the command's.
- Fixes RM-324 (tests/backlog/test_rm_found_2026_10_27.py::test_current_op_lists_a_bulk_write_under_its_op)
- Regression tests: `bulkwrite::tests::a_running_op_is_listed_in_mongods_shape`, `bulkwrite::tests::a_group_of_inserts_is_listed_as_one`, `diagnostics::tests::a_listing_pushed_for_an_op_is_popped_after_it`.
- Filed RM-391: an update or delete tests its filter against each document twice, once in the candidate read and again in the storage write (`tests/backlog/test_rm_found_2026_11_07d.py::test_a_write_tests_each_document_once`).
