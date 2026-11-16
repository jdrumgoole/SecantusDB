### A comma join's WHERE filters the join that first holds its tables

In a join of three or more tables, a WHERE conjunct over two of them now
appears in EXPLAIN as the Join Filter of the lowest join that holds both
tables, as PostgreSQL's planner evaluates it. Before, every such conjunct
was printed on the top join. A comma join linked only by inequalities is
now also priced: `explain select * from e, e e2, e e3 where e.a < e2.a and
e2.a < e3.a` prints PostgreSQL 15.19's costs, rows and Materialize nodes
instead of zeros.

#### Fixed

- EXPLAIN of a join of three or more tables places each WHERE conjunct
  over several tables on the lowest inner join holding them all, and the
  nested-loop chain estimate takes a comma join's WHERE clauses as its
  join clauses (RP-919, in part).

Fixes RP-919 in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations by a where])
