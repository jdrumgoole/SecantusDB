### `$$NOW` is bound inside a `$unionWith` pipeline

A `$unionWith` sub-pipeline reading `$$NOW` (in a `$documents` or a later
stage) now gets the command's date, as on mongod 8.2.11.

#### Fixed

- `$unionWith` passes the command's system variables (`$$NOW`,
  `$$CLUSTER_TIME`) into its sub-pipeline instead of an empty scope, which
  answered "$documents expression not supported".
  Fixes RM-655 (tests/backlog/test_rm_found_2026_11_14_rm531.py::test_now_is_bound_in_a_union_with_pipeline)
