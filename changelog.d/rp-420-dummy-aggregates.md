### EXPLAIN prices any plain aggregate over a relation that cannot return rows

A query whose WHERE folds to false, such as `select sum(a) from e where a = null`, plans an Aggregate over a `Result` with `One-Time Filter: false`. EXPLAIN on the Rust PostgreSQL server priced that Aggregate only when every aggregate was a count, and only over one table. It now prices `sum`, `avg`, `min`, `max`, `bool_and` and `bool_or` too, the way PostgreSQL 15.19's `cost_agg` does. Each result column is its type's average width, a final function named by `pg_aggregate` is costed once, and identical calls share their state. A count or other aggregate over a join made dummy is priced as well, and the Result keeps the width of the join columns the aggregate reads.

#### Fixed

- EXPLAIN of a plain aggregate over a dummy relation, or over a dummy join, prints PostgreSQL 15.19's estimate instead of zeros.

Fixes RP-420 (tests/backlog/test_rp_explain_costs.py::test_plan[a sum over a dummy relation is priced])
