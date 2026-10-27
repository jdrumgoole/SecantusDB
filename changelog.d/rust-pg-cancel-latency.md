### Rust PostgreSQL server: a cancel is answered as promptly as PostgreSQL answers one, however large the statement

PostgreSQL answers a cancel request at its next interrupt check, about 15 ms
after the request arrives. The Rust PostgreSQL server checks for a cancel
between planning passes. Over a statement with a 200,000-item `IN` list,
some passes ran for tens of milliseconds without a check, and the first
statement a process ran waited half a second while the built-in catalogs
were built. Every expression walk of a plan or Describe is now a
cancellation point too. The passes no longer copy the statement unless
they change it. The built-in catalogs and a database's catalog collections
are built at server start. On an optimised build, a cancel during the
planning of such a statement is answered within 10 ms; over 40 cancel times,
the worst was 33 ms and the median 14 ms. The statement also plans in about
half the time it took before.

Measuring that turned up two bugs, both fixed. A `pg_sleep` in the select
list of a streamed SELECT ignored a cancel and slept to the end. Every
index's `pg_class.relam` named B-tree, whatever its access method.

#### Changed

- `secantus-pgplan` (`cancel::with_walk_checks`): while a top-level
  statement is planned or described, `walk_expr` and `const_value` check for
  a cancel every 2,048 nodes. The caller's `check_cancel` straight after
  planning takes a cancel that cut a walk short, before the plan is used.
  Never active at execution. Fixes RP-202
  (`tests/backlog/test_rp202_cancel.py::test_a_cancel_while_a_huge_in_list_is_planned_over_a_table`).
- `secantus-pgplan`: `expand_views`, `fold_select`, `pg_expandarray`,
  `rowsfrom`, `column_alias_rewrite`, `record_fields`, `srf_tlist`,
  `whole_row_rewrite`, `expand_composite_star`, `enum_order`,
  `rewrite_xmlagg`, `rewrite_tablesample` and the `IN` list's evaluation
  check whether they apply before they copy the statement, reading it in
  place (`expr_any`, which visits what `walk_expr` visits).
- `secantus-pgplan` (`privileges`): the privilege pass's walks of a large
  statement run on a thread of their own, under the cancel hook.
- `secantus-pg`: the built-in catalogs are installed, and each database's
  catalog collections made, at server start. An Execute whose catalog has
  moved reads the statement through the planner's parse memo.

#### Fixed

- A `pg_sleep` in a streamed select list is cancellable. Fixes RP-286
  (`tests/backlog/test_rp202_cancel.py::test_a_sleep_in_a_streamed_select_list_is_cancelled`).
- `pg_class.relam` is the index's own access method. Fixes RP-285
  (`tests/backlog/test_rp_catalog_identity.py::test_an_index_names_its_access_method_in_relam`).
