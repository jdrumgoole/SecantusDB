### Rust PG server: a SQL function's bare parameter name gives way to a column

A `LANGUAGE sql` function's body had every identifier naming a parameter
rewritten to the parameter, wherever it appeared. So `gb(a int)` with the
body `select a from fe` read its argument instead of fe's column `a`, and
`select 1 as p` in a function with a parameter `p` was a syntax error. A
body's names are now resolved as PostgreSQL's parse analysis and
`sql_fn_post_column_ref` resolve them: a column reference is a column when
any relation in scope at its query level, or at an enclosing level, has
that column, and is the parameter only when none does. An output alias, or
anything else that is not a column reference, is never a parameter.

#### Fixed

- A SQL function's body reads a column, not a parameter of the same name.
  Each query level's namespace is built from its FROM items: a table's or
  a view's columns, a CTE's or a subquery's output columns (a `*`
  expanded), and a function's result columns (its OUT parameters, its
  composite type, or the one column of a scalar function). A subquery
  sees the levels around it, a non-LATERAL FROM-subquery does not see its
  siblings, an INSERT's VALUES does not see its target, and ORDER BY can
  name an output column. A utility statement is not analysed, as in
  PostgreSQL. `fname.p` (RP-451) and a composite parameter's field `p.f`
  (RP-462) are resolved by the same scopes.
  Fixes RP-461 (tests/backlog/test_rp_function_bodies.py::test_bare_parameter_name)
- Filed, found while working it: RP-478, a LATERAL subquery's bare name for
  an earlier FROM item's column is 42703
  (tests/backlog/test_rp_subquery_scope.py::test_lateral_subquery_reads_an_earlier_item_by_bare_name),
  and RP-477, a function with one OUT parameter names its FROM column after
  the function
  (tests/backlog/test_rp_function_bodies.py::test_one_out_parameter_names_the_column).
  A function body that names such a column now meets that 42703, where it
  read a parameter of the same name before.
