### An IN over a unique column explains as the inner join PostgreSQL plans

An `IN` or correlated `EXISTS` subquery whose inner column a unique index fixes -- `select * from e where a in (select a from k)` over `k`'s primary key -- is planned by PostgreSQL as an inner join, since each outer row can match at most once. EXPLAIN on the Rust PostgreSQL server now prints that `Hash Join`, priced and with the side PostgreSQL hashes, where it printed an unpriced sequential scan with an elided filter.

#### Fixed

- Rust PG server: EXPLAIN of an IN / EXISTS sublink over a unique inner column is the inner `Hash Join` `reduce_unique_semijoins` makes of it, costs included. Fixes RP-411, in part (tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery over a unique column is an inner join], tests/backlog/test_rp_explain_costs.py::test_plan[an exists subquery over a unique column is priced as an inner join]); the semi join's own costs stay open.
