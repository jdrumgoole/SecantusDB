### Rust PG server: a `schema.table.column` reference names the table in that schema

A column reference of three names had its schema ignored and was matched
by the table's name alone. `select other.fs.c from fs`, `select s.fs.c
from fs` and `update fs set c = other.fs.c` read `fs`'s column even though
the schema does not hold that table, and `select public.fs.* from fs` was
refused. Each now behaves as in PostgreSQL 15.19.

#### Fixed

- A three-name reference is resolved as PostgreSQL's `transformColumnRef`
  resolves it. `schema.rel` is looked up as a relation, and the reference
  finds that relation's entry only where the entry has no alias, searching
  the innermost query level first. When nothing matches, the 42P01 is
  `errorMissingRTE`'s, raised at the reference: `Perhaps you meant to
  reference the table alias "x".` for an aliased entry, `There is an entry
  for table "fs", but it cannot be referenced from this part of the
  query.` for any other entry, and `missing FROM-clause entry` when there
  is no entry. The query levels are modelled the way PostgreSQL builds
  them. A join's ON sees only its two sides, and an aliased join hides
  them. A FROM subquery sees the items before it only through LATERAL,
  while a function in FROM always sees them. An INSERT's source does not
  see its target, and a CTE is never the table of the same name. This
  covers SELECT, INSERT, UPDATE, DELETE, WITH, set operations, EXPLAIN,
  PREPARE, CREATE VIEW and cursors. Fixes RP-527
  (tests/backlog/test_rp_subquery_scope.py::test_schema_qualified_reference,
  tests/backlog/test_rp_subquery_scope.py::test_schema_qualified_reference_other_schema;
  crates/secantus-pgplan/src/schema_refs.rs::a_schema_qualified_reference_names_an_unaliased_relation).
- `select public.fs.* from fs` returns `fs`'s columns instead of 0A000.
  An INSERT's `RETURNING t.*` returns the target's columns instead of one
  composite column, as UPDATE and DELETE already did.
- Files RP-545 (a missing FROM relation is reported after the qualified
  references) and RP-546 (a function over a table's row type does not
  depend on the table).
