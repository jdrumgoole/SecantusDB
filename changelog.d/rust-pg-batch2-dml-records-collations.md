### Rust PostgreSQL server: multi-column UPDATE, RETURNING stars, records with char(n), ICU collations

A second batch of the Rust PostgreSQL server's backlog, each fix pinned by
the backlog test that measured it against PostgreSQL 15.19. Two of the
bugs it found on the way were silent wrong answers: inside a correlated
subquery an outer column took the type of the outer row's first column,
and `UPDATE ... FROM ... RETURNING *` left out the FROM table's columns.

#### Fixed

- `UPDATE ... SET (a, b) = (SELECT ...)` assigns each listed column from
  the subquery's one row: once per updated row, or once per statement when
  the subquery is uncorrelated, so a volatile subquery runs once as on
  PostgreSQL. No row sets every listed column NULL; two rows are 21000; a
  column count that differs is 42601. It works through `UPDATE ... FROM`,
  over a set operation, `*` and VALUES. Fixes RP-031
  (tests/backlog/test_rust_postgres_gaps.py::test_update_multi_column_assignment_from_a_subquery).
- `RETURNING t.*` on an UPDATE or DELETE, and `RETURNING *` over
  `UPDATE ... FROM` / `DELETE ... USING`, return the columns PostgreSQL
  does: the target's, then each FROM item's. `t.*` had been refused, and
  `*` had silently returned the target's columns only. A `q.*` naming no
  relation is 42P01. Fixes RP-034
  (tests/backlog/test_rust_postgres_gaps.py::test_update_from_returning_target_star).
- A record keeps a `char(n)` field's padding when it prints
  (`("ab ",1)`), in `row_to_json` and in `array_agg`, and still compares it
  without the padding. Fixes RP-032 for whole-row values
  (tests/backlog/test_rust_postgres_gaps.py::test_record_text_keeps_char_padding).
- `pg_collation` lists 880 of PostgreSQL 15.19's 907 ICU collations, each
  checked to sort as PostgreSQL does. A `<tag>-x-icu` name that is not one
  of them is 42704, as on PostgreSQL. Fixes most of RP-005
  (tests/backlog/test_rp_triage.py::test_pg_collation_lists_locale_icu_collations).
- Inside a correlated subquery, an outer column has its own type:
  `(select t.c = 'x' from s limit 1)` over a `char(3)` had compared an
  integer and answered false, and `pg_typeof` said `integer`.
- A subquery with no FROM that reads the outer row (`SET v = (SELECT v +
  1)`) runs as a correlated subquery instead of being refused.
- `UPDATE ... SET v = 1, v = 2` is 42601 `multiple assignments to same
  column`, as on PostgreSQL; it had applied the last.
- A row subquery that returns no row (`(select row(a, b) from t where
  false)`) is NULL instead of 42704 `type "record" does not exist`.
- `concat(t)`, `concat_ws` and `format('%s', t)` over a record print its
  text form, not the server's internal representation.
- `ROW(t.*)` is a row of t's columns, not a row holding t's row.
- An aggregate's `ORDER BY ... COLLATE "x"` naming no collation is 42704
  at the clause; it had sorted nothing.

#### Added

- `to_regnamespace`, `to_regrole`, `to_regcollation`, `to_regproc` and
  `to_regprocedure`; a dotted schema or role name is 42602 `invalid name
  syntax`. `regcollation` columns are described with their own type oid.
- Corpora `multi_assign_update`, `returning_star`, `icu_collations` and
  `to_reg_functions`.
