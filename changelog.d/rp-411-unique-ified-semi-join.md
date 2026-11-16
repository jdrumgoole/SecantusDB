### An IN subquery cheaper unique-ified is planned as PostgreSQL plans it

`EXPLAIN` of `select * from e where b in (select b from k)`, where making the subquery's rows unique first is the cheaper plan, now prints PostgreSQL 15's `Hash Join` hashing a `HashAggregate` of the inner table, grouped by the compared column, with its costs, instead of an unpriced `Hash Semi Join`. The semi join and the unique-ified inner join are priced against each other with the same semi-join factors PostgreSQL computes from the base relation, and the cheaper is printed.

#### Fixed

- A semi join cheaper as the inner join of its unique-ified inner side printed `Hash Semi Join (cost=0.00..0.00 rows=0 width=0)`; it is `Hash Join (cost=17.73..46.48 rows=1000 width=7)` over `Hash` over `HashAggregate`, `Group Key: k.b`.

Fixes RP-411, part (tests/backlog/test_rp_explain_costs.py::test_plan[a semi join cheaper unique-ified is a hash join over a hashaggregate])
