### Rust MongoDB server: index filters, `replSetReconfig`, every cluster parameter

Each was measured against mongod 8.2.11 and fixes RM-143
(`tests/backlog/test_rm_029_056.py::test_plan_cache_set_filter`,
`::test_resize_oplog_min_retention_hours`, `::test_every_cluster_parameter`,
`::test_repl_set_reconfig_keeps_the_new_configuration`).

#### Added

- **Index filters.** `planCacheSetFilter`, `planCacheListFilters` and
  `planCacheClearFilters` keep a filter per query shape: the query without
  its values, plus the sort, projection and collation. A `find`, `count`,
  `distinct`, `aggregate`, `update` or `delete` of that shape plans over
  the filter's indexes and the collection scan only, and its `explain`
  says `indexFilterSet: true`. Filters live in memory, as mongod keeps
  them, and end when the collection is dropped. A 28-scenario comparison
  agrees except for list order, which is mongod's hash order.
- **`replSetReconfig`** for the one-member set: mongod's version, name,
  member, priority, vote, host and `replicaSetId` checks. The settings are
  rebuilt from their defaults, and the configuration is kept in
  `local.system.replset` across restarts (17 scenarios, no difference). A
  configuration of more than one member is refused: this is a single-node
  set.
- **`replSetResizeOplog` takes `minRetentionHours`.** No oplog entry
  younger than that is pruned, whatever the entry cap. It is a floor in the
  shared storage crate's oplog prune, which the PostgreSQL server links
  too.
- **`getClusterParameter`** lists all fifteen of mongod's parameters at
  their defaults; it had six.
