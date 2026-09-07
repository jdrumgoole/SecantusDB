### Composite types: the DDL and the catalog

`CREATE TYPE name AS (field type, ...)` and `DROP TYPE` work now, written in the
same `__sql_composites__` catalog the Python server uses — a doc per type with
its ordered fields and a monotonically minted oid (base 67000, the enum rule),
so an enum created by one server resolves on the other under the same oid.

Composites appear in `pg_type` (with `typrelid` set to their own oid),
`to_regtype` and `regtype`, and a new `pg_attribute` virtual table exposes each
composite's fields keyed on that oid — `attname`, `atttypid`, `attnum`. This is
the catalog `CompositeInfo.fetch` reads; its query's nested-subquery join is a
separate slice (a JOIN whose right arm is itself an aggregate subquery), so the
fetch itself does not resolve yet.

#### Added

- `CREATE TYPE … AS (…)`, `DROP TYPE` for composites, composites in
  `pg_type`/`to_regtype`/`regtype`, and the `pg_attribute` virtual table.
