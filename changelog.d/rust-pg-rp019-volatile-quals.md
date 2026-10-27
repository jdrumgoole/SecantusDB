### Rust PostgreSQL server: a volatile condition runs once for each row of its table, as in PostgreSQL's scan

PostgreSQL never folds a volatile function such as `nextval` or `random` into
a constant. It evaluates a condition over one table in that table's scan,
once per row, even when the query joins the table to others. The Rust
PostgreSQL server folded a volatile call in a WHERE into a constant while
planning, so it ran once. In a join it ran the condition once per joined
row, and inside a transaction block `nextval` on a sequence the block had
created was refused as a missing relation. Such a condition now runs per
row, and in a join it is moved into its table's scan rather than copied
there. The sequence counts match PostgreSQL 15.19's for each of these
shapes.

#### Fixed

- `secantus-pgplan` (`lower_where_nonvolatile`): a WHERE condition that
  calls a volatile function (`pg15_volatile_functions.tsv`, or a user
  function declared or left VOLATILE) is evaluated per row, never lowered
  into a filter built once.
- `secantus-pgplan` (`joins::pushdown_filters`): a conjunct over one table
  that calls a volatile or stable built-in or a user function is moved into
  that table's leaf and left out of the join's WHERE. A copy stays only for
  immutable conjuncts. Fixes RP-019
  (`tests/backlog/test_rp_top10_found.py::test_a_volatile_condition_runs_once_a_row_of_its_table`).
- `secantus-pgplan` (`UserFn::volatile`): a user function's volatility
  reaches the planner.
