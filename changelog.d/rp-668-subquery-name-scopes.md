### Rust PG server: a name bound inside a derived table no longer hides the outer one

A subquery that read the enclosing query's row by a table name it also used
inside one of its own FROM-subqueries was refused. `update t set a = x.a
from (select a from t where b < 0) x where t.b < 0` answered 42P01 `missing
FROM-clause entry for table "t"` where PostgreSQL updates the row, and so
did the same shape in a SELECT's scalar or EXISTS subquery, a `DELETE ...
USING`, a derived table that is a `UNION`, and a `LATERAL` one.

PostgreSQL's namespace is per query level: a table named inside a derived
table, a set operation's arm or a nested subquery is visible only there, so
the same name beside it is the enclosing query's. The server now finds a
subquery's outer references the same way, level by level, and substitutes
only those, so a reference the derived table qualifies with its own table
keeps reading that table.

#### Fixed

- `secantus-pgplan` (`correlated.rs`): a correlated subquery's qualified
  outer references, and the `$N` parameters that replace them, are found
  by a scope-aware walk (`walk_select_scoped`) instead of against every
  name bound anywhere inside the subquery; `foreign_qualifier`'s check of
  a derived table now covers each UNION arm separately and LATERAL
  derived tables (`derived_outer_qualifier`).
- Fixes RP-668 (tests/backlog/test_rp_found_rp664.py::test_update_from_subquery_over_the_target,
  tests/backlog/test_rp_found_rp664.py::test_a_name_bound_inside_a_derived_table_is_not_bound_beside_it).

#### Filed

- RP-686: RETURNING a FROM item's column is still refused (0A000) when the
  UPDATE's join condition reads a column it assigns
  (tests/backlog/test_rp_found_rp664.py::test_returning_a_from_item_when_the_join_reads_an_assigned_column).
