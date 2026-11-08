### Rust PG server: a string literal given as a `regclass` is looked up when the statement is analysed

PostgreSQL turns an untyped literal into a `regclass` as it analyses the
statement (`coerce_type` runs `regclassin`), so a literal naming a relation
that does not exist is refused before anything runs, wherever it sits. This
server looked the relation up only if the call ran, so a statement
PostgreSQL refuses could run here: `select 1 where false and
nextval('nosuchseq') > 0` returned no rows, and a subquery that yielded no
row returned a NULL. The analysis pass now reads each such literal in the
order PostgreSQL's transform meets it. That covers an argument in a
`regclass` position of the built-in a call resolves to (`nextval`,
`pg_relation_size`, ...), an explicit cast (`'t'::regclass`), and the call
form `regclass('t')`. A missing FROM relation, or a column reference earlier
in the statement that does not resolve, is still reported first.

#### Fixed

- `select 1 where false and nextval('nosuchseq') > 0`, `select (select
  nextval('nosuchseq') where false)`, a CTE nothing reads, a CASE arm never
  taken, `ORDER BY nextval('nosuchseq')` and `EXPLAIN` of any of them are
  42P01 `relation "nosuchseq" does not exist` at the literal, as on
  PostgreSQL 15.19. So is `CREATE FUNCTION ... LANGUAGE sql` over a body
  that names one.
- The 42P01 now comes ahead of the errors PostgreSQL reports after it: a
  subquery's width (`select (select nextval('nosuchseq'), 2)`), an operator
  with no overload (`nextval('nosuchseq') + true`) and a missing column to
  its right (`select nextval('nosuchseq'), nosuch`).

Fixes RP-458 (tests/backlog/test_rp_regclass_literal.py::test_regclass_literal_is_resolved_at_analysis, tests/backlog/test_rp_regclass_literal.py::test_regclass_literal_in_analysis_order)

Filed while working it: RP-675 (`regclassin` names a missing schema as a
missing relation), RP-676 (the sequence functions over a relation that is
not a sequence), RP-677 (a named argument to `nextval`).
