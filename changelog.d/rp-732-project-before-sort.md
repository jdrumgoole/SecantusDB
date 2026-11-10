### Rust PG server: a query with ORDER BY computes its select list below the Sort

PostgreSQL's Sort reads rows that already carry the select list, computed
once a row in scan order, so `select k, ('x' || k)::int from v order by k
desc` raises the first scanned row's error, and a row a LIMIT drops still
raises one. The Rust PG server computed the select list after sorting, as
the rows were sent: the row that sorted first raised its error, and a row
past the LIMIT never ran.

#### Fixed

- A select list is split the way PostgreSQL 15.19's
  `make_sort_input_target` splits it (`secantus-pgplan`'s `sort_input.rs`).
  A column that is no sort key and is volatile is postponed past the Sort.
  So is an expensive one (over 10 x `cpu_operator_cost`, priced from
  `pg_proc.procost` the way `cost_qual_eval` prices it) when the query has
  a LIMIT. Every other column is computed below the Sort, a row at a time
  with its ORDER BY keys, and rides the row so it is not computed again
  when the row is sent. A DISTINCT's computed columns ride the row the
  same way, so the value printed is the one compared.
- Fixes RP-732 (tests/backlog/test_rp_found_rp730.py::test_select_list_is_computed_before_the_sort)

#### Found

- RP-744: a DECLARE CURSOR computes every row's select list at the DECLARE.
- RP-745: a sorted cursor does not postpone an expensive column
  (`cursor_tuple_fraction`).
- RP-746: a cast over a CASE of untyped literals reports a branch's input
  error at the literal.
