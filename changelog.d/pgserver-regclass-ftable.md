### The Rust PostgreSQL server names each result column's source table

A `RowDescription` now says where each column came from: the relation oid and
1-based attribute number libpq exposes as `PQftable` / `PQftablecol`, through
an alias, a two-table `FROM`, a `JOIN`, `SELECT *` and `INSERT ... RETURNING`,
over the simple and the extended protocol alike. A computed column reports
`0` / `0`, exactly as PostgreSQL 16 does. The relation oid is the one the
table's row type already carried in `pg_type` and `pg_attribute.attrelid`, so
the three agree.

The `regclass` type arrives with it: `'t1'::regclass` resolves a relation name
to that oid (unquoted parts fold to lower case, quoted ones keep theirs, a
`public.` / `pg_temp.` / `pg_catalog.` prefix is honoured, the fixed catalog
relations such as `pg_class` answer their PostgreSQL oids), renders as the
name, casts to `oid` / `int` / `text`, and refuses what PostgreSQL refuses
with the same codes -- `42P01` for an unknown relation, `42602` for a
malformed name, `42846` for a cast to `regtype`. A comma-separated `FROM t1,
t2` plans as a CROSS join, and a select list may repeat an output name.

#### Added

- `crates/secantus-pgplan`: the `regclass` type (oid 2205) -- resolution
  against the session's published relations, text rendering, casts to and
  from it, and PostgreSQL's error surface; comma-FROM cross joins; constant
  targets in a join's select list.
- `crates/secantus-pgserver`: `RowDescription` `ftable` / `ftablecol` for
  pass-through base-table columns; each table's columns carry their source
  from the catalog lookup and from `CREATE TABLE` within the transaction.

#### Fixed

- `crates/secantus-pgserver`: two join outputs with the same name (`select
  'a'::regclass::oid, 'b'::regclass::oid from t1, t2`) no longer collapse to
  one value.
