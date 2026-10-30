### Rust PG server: `nextval` in a join's select list runs once a joined row

A two-table join whose select list called a volatile function such as
`nextval('s')` ran it once while planning, as though it were a constant:
`select a.a, nextval('s') from a join b on a.a = b.b` answered the same
value on every row, and the comma-join form drew one value too many before
the general planner ran it per row. The join path now leaves a volatile
target to the planner that evaluates it once a joined row, as PostgreSQL
15.19 does.

#### Fixed

- `nextval` (and any other volatile call) in a two-table join's select list
  is no longer folded or run while planning.

Fixes RP-288 (tests/backlog/test_rp_top10_found.py::test_a_select_list_nextval_in_an_autocommit_join_runs_once_a_row)
