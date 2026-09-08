### Custom range types expose their auto-created multirange companion

`CREATE TYPE testrange AS RANGE (...)` on the Rust PostgreSQL server now creates
the companion MULTIRANGE type that PostgreSQL auto-creates alongside every range
type, and psycopg's `MultirangeInfo.fetch` resolves it. Previously the companion
was entirely absent: `MultirangeInfo.fetch(conn, "testmultirange")` errored
`column "rngmultitypid" does not exist`, because `pg_range` had no
`rngmultitypid` column, no per-range multirange oid was minted, and no
multirange `pg_type` row existed.

The multirange's name follows PostgreSQL's rule — substitute `multirange` for
the first `range` in the range's name (`testrange` → `testmultirange`,
`int4range` → `int4multirange`, `rangetest` → `multirangetest`), or append
`_multirange` when the name contains no `range` (`foo` → `foo_multirange`). Each
custom range mints a multirange oid and a multirange array oid, gets a
multirange row in `pg_type` (its `typname` bare, as in PostgreSQL), and a
`pg_range.rngmultitypid` pointing back at it. `to_regtype` resolves the
multirange name in bare, `schema.name`, and quoted `sql.Identifier` forms, and a
schema-qualified range's multirange is distinct from the public one's — mirroring
the range schema-qualification from the previous release. All four
`MultirangeInfo.fetch` forms resolve to the right oid and subtype, verified
against a live PostgreSQL 14 oracle.

#### Added

- `pg_range` carries a `rngmultitypid` column, populated for both builtin ranges
  (pointing at the builtin `int4multirange`/etc. types) and custom ranges
  (pointing at a minted multirange oid, `range_oid + 200_000`; its array type
  `+ 300_000`).
- A multirange `pg_type` row is synthesized for every custom range, with a bare
  `typname` derived by PostgreSQL's `range`→`multirange` naming rule and its own
  array oid, so `MultirangeInfo.fetch` finds it after resolving `to_regtype` and
  joining `pg_range`.
- `to_regtype` resolves a custom multirange name (bare in `public`,
  `schema.name` otherwise), distinct per schema; the reverse `oid::regtype::text`
  rendering resolves a multirange oid back to its name.
