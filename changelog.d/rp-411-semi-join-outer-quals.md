### An IN or EXISTS beside a filter on the outer table is a join

`EXPLAIN select * from e where b in (select b from k) and a > 5` on the Rust
PostgreSQL server printed a bare, unpriced scan of `e`. PostgreSQL pulls the
sublink up into a join whatever plain conjuncts sit beside it, and they
filter the outer table's scan. The server now does the same: the semi join,
or the inner join it is cheaper or unique as, is drawn and priced with the
outer scan filtered by those conjuncts.

#### Fixed

- Rust PG server: an `IN` / `= ANY` / `EXISTS` sublink ANDed with conjuncts
  over the outer relation that its scan's filter holds is planned as the
  semi join (or its unique-ified or unique inner join), the outer scan
  filtered and priced. Fixes RP-411 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[a sublink anded with an outer qual is planned as a join]).
