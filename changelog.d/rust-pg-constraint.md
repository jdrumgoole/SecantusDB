### The Rust PostgreSQL server answers `pg_constraint`, and a regclass list no longer silently matches nothing

`pg_constraint` was the last catalog table a client could not read on the Rust
PostgreSQL server: every query against it answered `42P01 relation
"pg_constraint" does not exist`, even though the server already enforced all
four constraint kinds and recorded each one's name. It is now a virtual table
with PostgreSQL 14's full 25-column shape, projected from the catalog the
server already keeps.

Measuring it turned up a second, quieter bug one level down. A `regclass` value
is carried as a document holding its oid, and only the scalar comparison path
unwrapped it — so `WHERE conrelid IN ('t'::regclass, 'u'::regclass)` compared
documents against numbers, matched **nothing**, and returned zero rows with no
error, while the same predicate spelled with `OR` returned the right ones. That
is the shape catalog reflection actually emits (SQLAlchemy and pgjdbc both use
`IN` / `= ANY`), so it read as "this server has no constraints" rather than as a
defect.

#### Added

- `pg_constraint` on the Rust PostgreSQL server, with PostgreSQL 14.24's column
  set, attnum order and wire-type oids — including the two types the server had
  no vocabulary for: the internal single-byte `"char"` (18) that `contype` and
  the three `conf*type` columns use, and `pg_node_tree` (194) for `conbin`.
  Rows cover PRIMARY KEY (`p`, named `<table>_pkey` as PostgreSQL names an
  implicit one), UNIQUE (`u`), `EXCLUDE` (`x`), CHECK (`c`) and FOREIGN KEY
  (`f`) with its one-letter `ON UPDATE` / `ON DELETE` action codes and
  `confkey` resolved against the parent table.
- `pg_constraint` is listed in `pg_tables` under `schemaname = 'pg_catalog'`,
  like the other catalog relations the server answers for.

#### Fixed

- A `regclass` or `regtype` operand inside `IN (...)` or `= ANY(ARRAY[...])` is
  now compared by its oid, as the scalar `=` path already did. Previously such
  a predicate matched no rows at all and reported no error.

#### Notes

- **NOT NULL deliberately produces no row.** PostgreSQL records a not-null as
  `pg_attribute.attnotnull` rather than as a `pg_constraint` entry, so a table
  with a NOT NULL column, a CHECK, an FK, a UNIQUE and a PK has exactly five
  rows here — measured, and pinned by a test.
- Two columns are honestly empty rather than fabricated: `conbin` is NULL
  because the server keeps a CHECK predicate as SQL text, not as PostgreSQL's
  serialised parse tree, and `conindid` is 0 because there are no `pg_index`
  rows for it to point at. Constraint `oid`s are synthetic — distinct and
  stable per table, but not PostgreSQL's numbers.
