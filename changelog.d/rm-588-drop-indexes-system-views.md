### A `dropIndexes` of `system.views` answers as mongod's does

The Rust MongoDB server refused a `dropIndexes` of `<db>.system.views` with
NamespaceNotFound, though a view had made it. It now answers as mongod 8.2.11
does: `index: "*"` succeeds with `nIndexesWas: 1`, `_id_` is refused as the
`_id` index, and any other index is not found.

#### Fixed

- `dropIndexes` of `system.views` while a view exists.
  Fixes RM-588 (tests/backlog/test_rm_view_ddl_refusals.py::test_a_drop_indexes_of_system_views_answers)
