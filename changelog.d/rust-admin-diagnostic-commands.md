### The admin and diagnostic commands of a one-member replica set

The Rust MongoDB server answers the commands a tool sends a one-member
replica set as mongod 8.2.11 does, over a probe of 74 replies where 3 still
differ (`features` has no `js` block because there is no script engine,
and an oplog query that reads state an earlier run left). Two silent
differences found on the way are fixed too: a write to the `local` database
went into the oplog, and an inclusion projection in `find` returned fields
sorted by name.

#### Added

- `replSetInitiate` (23, or 76 on a standalone), `appendOplogNote` (a no-op
  oplog entry), `replSetResizeOplog` (the oplog's cap, 990 MB to 1 PB, with
  mongod's range messages), `lockInfo`, `features`, `getClusterParameter`
  (the parameters whose default is a fact here), `planCacheClear`,
  `planCacheClearFilters` and `planCacheListFilters` (there is no plan cache
  and no index filter).
- `local.oplog.rs` is listed, with mongod's options and no index, and the
  set's configuration is kept in `local.system.replset`, so its
  `replicaSetId` is the same after a restart. `replSetGetConfig` reads it.
  `admin.system.version` holds the feature compatibility version from the
  first start, so `listDatabases` always lists `admin`.
- `replSetGetStatus` has `electionCandidateMetrics`.
- `setParameter` changes `cursorTimeoutMillis` and
  `transactionLifetimeLimitSeconds` while the server runs, applied to the
  cursor and transaction registries, with mongod's coercion and messages.
- `hostInfo` reports the machine's own figures (memory, cores, sockets,
  architecture, OS release; `extra` on macOS).
- `validate` reads `system.views`; `dbStats` counts a view's definition in
  `dataSize`.
- A `saslStart` or `saslContinue` payload sent as a base64 string is
  decoded and answered in base64.

#### Fixed

- A write to the `local` database is no longer written to the oplog, so a
  change stream does not see it; a retryable write to `local` fails each
  statement that would write with 20, as on mongod.
- `find` with a top-level inclusion projection returns the fields in the
  stored document's order, not sorted by name. The full projection path
  was fixed for this in 2026-09; the raw fast path beside it was not.
- `authenticate` and `saslStart` over MONGODB-X509 without a certificate
  say only "Authentication failed.", as every failed authentication does.
- Fixes RM-036 (`tests/backlog/test_rm_029_056.py::test_replica_set_commands`,
  `::test_replica_set_internal_collections`) and RM-037
  (`::test_commands_not_there`, `::test_admin_only_command_parses_its_argument_first`),
  and parts of RM-033 (`::test_validate_system_views`), RM-035
  (`::test_set_parameter_at_runtime`) and RM-040
  (`::test_host_info_system_fields`, `::test_db_stats_counts_view_definitions`,
  `::test_x509_without_a_certificate`, `::test_sasl_start_base64_payload`),
  each now a `backlog_fixed` regression test. Pinned besides by
  `crates/secantus-storage-adapter/tests/replset_admin_wt.rs`.
- Filed RM-143: the commands answered only as far as this server honours
  them (`features.js`, `oidReset`, the other cluster parameters,
  `planCacheSetFilter`, `minRetentionHours`, `replSetReconfig`).
