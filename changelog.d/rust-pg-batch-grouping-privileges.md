### Rust PostgreSQL server: built-in EXECUTE checks, grouping, search_path and error positions

Seven divergences from PostgreSQL 15 closed, each pinned by the backlog
test that measured it.

#### Fixed

- `REVOKE EXECUTE` on a built-in function is enforced wherever PostgreSQL
  enforces it: inside a view, a rule's action, a column DEFAULT an INSERT
  fills, a nested call (`abs(length('ab'))`), and over a VALUES or CTE
  column. Fixes RP-004
  (tests/backlog/test_rp_triage.py::test_revoked_builtin_refused_through_view_and_default,
  tests/backlog/test_rp_triage.py::test_revoked_builtin_refused_where_the_overload_is_unanalysed).
- A parenthesised step inside `ROLLUP` / `CUBE` (`rollup ((g, z), v)`) is
  added and dropped as one. Fixes RP-008
  (tests/backlog/test_rust_postgres_gaps.py::test_rollup_with_a_parenthesised_step).
- A grouped query's subquery can name the outer table by its own name
  when the subquery aliases the same table. Fixes RP-009
  (tests/backlog/test_rust_postgres_gaps.py::test_grouped_query_subquery_names_outer_table).
- A 42803 raised from HAVING names the column with its table, and a
  "subquery uses ungrouped column" error carries its position. Fixes RP-010
  (tests/backlog/test_rust_postgres_gaps.py::test_having_42803_names_the_table).
- With `public` off the `search_path`, `nextval('s')`, `currval('s')` and
  `'t'::regclass` no longer find a relation of `public`; `DROP ... IF
  EXISTS` skips it with PostgreSQL's notice instead of dropping it; and a
  regclass of `public` prints qualified. Fixes RP-011
  (tests/backlog/test_rust_postgres_gaps.py::test_public_off_path_not_found_from_a_string,
  tests/backlog/test_rust_postgres_gaps.py::test_public_off_path_drop_if_exists_skips).
- `DROP TABLE / VIEW / MATERIALIZED VIEW / SEQUENCE IF EXISTS s.x` names
  `x`, not `s.x`, in its notice, and the sequence form sends one at all.
- An error found while describing a statement (Parse + Describe) carries
  the same position it does through a simple query. Fixes RP-012
  (tests/backlog/test_rust_postgres_gaps.py::test_parse_syntax_error_carries_position).
- `SET TRANSACTION` outside a transaction block sends PostgreSQL's warning.
  Fixes RP-001
  (tests/backlog/test_rp_triage.py::test_set_transaction_outside_a_block_warns).
