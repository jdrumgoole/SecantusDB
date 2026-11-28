### Rust PG server: a WHERE equality between expressions over two tables is a priced join, as its ON form is

PostgreSQL makes an inner join's ON and WHERE one list of quals
(`deconstruct_jointree`), so an equality between an expression over one
table and one over the other is a merge or hash clause wherever the query
wrote it. The server priced it only in an ON. A comma join or CROSS JOIN
whose WHERE held one printed an unpriced Nested Loop with a Join Filter.
That clause now joins the two tables as the ON form does, with the WHERE's
column equalities beside it. The rows returned do not change.

#### Fixed

- `explain select * from e, e e2 where e.b = e2.b || 'x'` (and the same
  over `cross join`) prints PostgreSQL 15's `Merge Join
  (cost=129.66..224.66 rows=5000 width=14)` over two sorted scans.
- `explain select * from e where a + 1 in (select a from k)`, over a
  unique `k.a`, prints the `Hash Join (cost=27.50..45.26 rows=1000
  width=7)` hashing `k`.
- `explain select e.b from e, k where e.a = k.a - 1 and e.a = k.a` makes
  the equivalence class its ON form makes: `Hash Cond: (e.a = k.a)`, with
  `k` filtered by `((a - 1) = a)`.

Fixes RP-1127 (tests/backlog/test_rp_explain_costs.py::test_plan[an expression equality in a comma join's where is a merge join], tests/backlog/test_rp_explain_costs.py::test_plan[an in of an outer expression over a unique column is a hash join])
Fixes RP-1171, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a comma join's expression equality joins its class])
