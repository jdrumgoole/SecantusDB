### Docs: three-way feature comparison (MongoDB vs Python server vs Rust server)

A new [Feature comparison](https://secantusdb.readthedocs.io/en/latest/feature-comparison.html)
docs page decomposes the validation-report pass rates into a per-feature
matrix: commands, query/update/expression operators, aggregation stages,
accumulators and window functions, index types, collections, change streams,
transactions, auth, backup/PITR, and the SQL frontend — each marked
supported / partial / missing for real `mongod`, the Python server, and the
Rust server.

#### Changed

- `docs/servers.md`: refreshed the stale "what the Rust server doesn't
  support" list — the pymongo-suite gap has closed to parity (99.5% both) and
  the DDL-change-stream-event, large-event-splitting, and timeseries-`_id`
  bullets described already-shipped features; replaced with the current gap
  set (SQL frontend, `mapReduce`/`top`, wire-level `restoreToTimestamp`,
  session lifecycle no-ops, oracle-deferred operator edges, thinner
  diagnostics). Dropped the out-of-scope claim that RBAC is unimplemented
  (both servers enforce it).
- `docs/index.md`: added the new page to the toctree and quick links, and
  included the previously-orphaned psycopg validation report in the toctree
  (it was failing the `-W` docs build as `toc.not_included`).
