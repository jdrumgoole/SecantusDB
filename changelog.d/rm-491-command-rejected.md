### Rust MongoDB server: `metrics.commands` counts rejected commands

`serverStatus`'s `metrics.commands.<name>` now carries `rejected` beside
`failed` and `total`, as mongod 8.2.11's does. A read a query setting refuses
with `reject: true` (411 `QueryRejectedBySettings`) counts in its command's
`rejected` and `total` and no longer in `failed`.

#### Fixed

- `metrics.commands.<name>` had no `rejected`, and a query-settings refusal
  counted as a failure.
  Fixes RM-491 (tests/backlog/test_rm_view_namespaces.py::test_a_command_metric_counts_rejections,
  tests/backlog/test_rm_view_namespaces.py::test_a_query_settings_refusal_counts_rejected_not_failed)
