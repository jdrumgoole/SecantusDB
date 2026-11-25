### EXPLAIN removes a LEFT JOIN nothing reads

A LEFT JOIN whose inner side is one table, unique on the columns its ON equates to the outer side or to constants, and read nowhere but in that ON, is now planned away, as PostgreSQL's `remove_useless_joins` does: `explain select e.b from e left join k on e.a = k.a` (`k.a` the primary key) prints `Seq Scan on e` alone, priced as that scan. The uniqueness is proven from the table's unique indexes (immediate, without a predicate, every key fixed), across integer widths, over an expression of the outer side, by a USING column, and for several such joins in one query. The removed relation stays in the range table, so `Group Key`, `Sort Key` and VERBOSE `Output` print prefixed, as PostgreSQL prints them.

#### Fixed

- `EXPLAIN` of a LEFT JOIN that nothing above it read, to a table unique on its ON's columns, printed the join. Fixes RP-1060 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join nothing reads is removed]); its regression scenarios are `tests/backlog/test_rp_explain_costs.py::test_plan[a left join to a unique index's table nothing reads is removed]` and the RP-1060 scenarios beside it, and `join_removal::rp1060_join_removal` in `secantus-pgplan`.
- Filed in passing: RP-1106 (a LEFT JOIN to a view or a one-row CTE is not removed), RP-1107 (a GROUP BY over a cross join with a FROM subquery prints a joined-row key) and RP-1108 (a Sort above a HashAggregate prints no Sort Key).
