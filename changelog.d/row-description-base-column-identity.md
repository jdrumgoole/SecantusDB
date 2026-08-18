### Result columns keep their identity across joins, views, and retypes

Every row Postgres describes on the wire says where each column came from: the
relation it belongs to and its position within that relation. Tools lean on
that. A JDBC updatable `ResultSet` resolves a result column back to its base
column through those two fields, and reporting nothing left `updateRow()`
emitting broken SQL. SecantusDB filled them in for a single-table `SELECT`, but
a join has no single base table, so every column of a joined result reported no
provenance at all. Joined columns now carry their own table's identity — in
`SELECT tab1.a, tab2.c`, both columns are the first column of their own table
and both say so, and a computed expression alongside them no longer strips the
identity from its plain siblings.

Views got the same treatment, and fixing them turned up a worse bug underneath.
A view declared with a column list — `CREATE VIEW v (v1, v2) AS SELECT …` — was
filed in the catalog under an empty name while `CREATE VIEW` still reported
success, so every later reference to it failed as an undefined relation. The
declared names are now applied to the stored definition the way Postgres applies
them (positionally, surplus outputs keeping their own names, more names than
columns raising `42601`), a `SELECT *` body has its columns resolved once at
creation, and a view's result columns report the view's own oid and its own
positions rather than the underlying tables'.

Two smaller fidelity gaps closed alongside them. A `char(n)` value now goes out
blank-padded to its declared width, as a blank-padded type should, while the
semantics that read it — `length()`, comparison, casting to `text` — still see
it unpadded, matching Postgres on both halves. And `ALTER TABLE … ALTER COLUMN …
TYPE` now recomputes the column's declared type identity instead of inheriting
the old one: a column retyped from `char(8)` to `text` kept describing itself as
`bpchar` with the old width, phantom padding included.

#### Added

- Result columns from a JOIN report their source table's oid and 1-based
  attribute number, resolving qualified (`tab1.a`) and unqualified column
  references alike, including through `SELECT *`.
- Result columns selected from a view report the view's own pg_class oid and the
  column's position within the view.
- `CREATE VIEW v (cols…)` applies the declared column names to the stored
  definition, resolving a `SELECT *` body's columns at creation time (so a
  column added to the underlying table afterwards does not appear in the view,
  as in Postgres).

#### Fixed

- `CREATE VIEW` with a column list registered the view under an empty name while
  still reporting success; every later reference failed with `relation "v" does
  not exist`.
- `CREATE VIEW` with more column names than the query has columns now raises
  `42601 CREATE VIEW specifies more column names than columns` instead of
  silently ignoring the surplus.
- `char(n)` values are blank-padded to the declared width on the wire.
- `ALTER TABLE … ALTER COLUMN … TYPE` now replaces the column's declared type
  oid and modifier; retyping away from `char(n)`/`varchar(n)` left the old
  declaration in place, so the column kept describing itself as the old type.
