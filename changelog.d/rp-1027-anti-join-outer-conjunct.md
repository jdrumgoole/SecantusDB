### Rust PG server: an anti join beside an outer conjunct is a priced Hash Anti Join in EXPLAIN

`EXPLAIN` of a `NOT EXISTS` beside a condition on the outer table (`select b
from e where a > 500 and not exists (select 1 from e e2 where e2.a = e.a)`),
and of its LEFT JOIN spelling (`... left join e e2 on e.a = e2.a where e.a >
500 and e2.a is null`), now prints PostgreSQL's plan: a priced `Hash Anti
Join` whose outer `Seq Scan` carries the condition as its `Filter`, with the
rows it keeps.

#### Fixed

- The NOT EXISTS rewrite into the anti join's LEFT JOIN form keeps the outer
  conjuncts as the WHERE's, over the outer relation, instead of giving up on
  them; a conjunct naming a column it cannot qualify is left as written.
- A LEFT JOIN whose WHERE tests the nullable side's join key `IS NULL` is
  recognised as an anti join beside conjuncts over the preserved side that
  its scan applies, `<>` included, and prices that scan's filter.

Fixes RP-1027 (tests/backlog/test_rp_explain_costs.py::test_plan[a not exists beside an outer conjunct is a hash anti join], tests/backlog/test_rp_explain_costs.py::test_plan[a left join anti join beside an outer conjunct is a hash anti join]). Files RP-1134: an outer OR, IN list or expression comparison beside the anti join is still unpriced.
