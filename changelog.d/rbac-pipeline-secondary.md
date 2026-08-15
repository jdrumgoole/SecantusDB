### Aggregation pipelines can no longer sidestep RBAC

With access control enabled, aggregate's privilege check covered only
the primary collection — a principal holding nothing but `find` on one
collection could overwrite any namespace in any database via `$out` or
`$merge`, and read foreign namespaces via the `$lookup` family, with no
grant on the target. Both servers now resolve a pipeline's
secondary-namespace requirements before execution, the same model
mongod uses: `$out` demands insert+remove on its target, `$merge`
insert+update, and `$lookup` / `$graphLookup` / `$unionWith` demand
find — with sub-pipelines and `$facet` branches walked recursively.
An unauthorized stage is rejected with `Unauthorized` (13) before the
pipeline touches anything.

`configureFailPoint` — a server-wide fault-injection lever that could
close every client's connection — previously required no privilege at
all under `--auth`. It now demands a cluster-admin grant on both
servers, mirroring mongod's rule that test commands require a
privileged role.

#### Security

- aggregate `$out`/`$merge` could write to (drop and replace) any
  namespace, and `$lookup`/`$graphLookup`/`$unionWith` could read any
  same-db namespace, with only a `find` grant on the primary collection
  (#783). Both servers now check per-stage privileges pre-execution.
- `configureFailPoint` was missing from both servers' RBAC action
  tables, so any authenticated principal — even one with zero roles —
  could arm a server-wide DoS failpoint (#806). It now requires a
  cluster-admin grant (`clusterAdmin` or `root`).
