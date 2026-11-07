### Rust PG server: a quoted literal in a WHERE, ON or CASE WHEN reads as a boolean

PostgreSQL gives a string literal the type UNKNOWN. Where a clause must be
boolean, `coerce_to_boolean` reads the literal through `boolin` during parse
analysis, so `where 't'` keeps every row and `where 'x'` is 22P02. Here the
literal stayed a string. A WHERE, HAVING, FILTER or AND / OR argument answered
42804 `argument of WHERE must be type boolean, not type text`. A JOIN's ON and
a searched CASE's WHEN were worse: they matched no row and gave a wrong answer
with no error.

#### Fixed

- `secantusd-pg`: an untyped string literal in a WHERE (a subquery's, a
  CTE's, an UPDATE's and a DELETE's included), a HAVING, a JOIN's ON, an
  argument of AND / OR / NOT, a searched CASE's WHEN or an aggregate's
  FILTER is the boolean `boolin` reads from it, as on PostgreSQL 15.19. A
  stored view prints it as `true` / `false`, and EXPLAIN folds it. A literal
  that is no boolean is 22P02 `invalid input syntax for type boolean` at the
  literal, after a missing relation's 42P01.
- A text-to-boolean cast now reads what PostgreSQL's `parse_bool_with_len`
  reads: any case-blind prefix of `true`, `false` or `yes` (`'tr'`, `'fal'`,
  `'ye'`), and `of` for `off`. Each of these was 22P02 here.

Fixes RP-424 (tests/backlog/test_rp_where_types.py::test_where_untyped_literal_reads_as_boolean,
tests/backlog/test_rp_where_types.py::test_untyped_literal_in_every_boolean_position).
