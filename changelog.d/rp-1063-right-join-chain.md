### Rust PG server: a RIGHT JOIN atop a join of two tables is planned by the join order search

Over tables never analysed, a RIGHT JOIN atop a chain of two tables
(`... join k on e.a = k.a right join k k2 on k2.a = e.a`) is now planned
as PostgreSQL plans it. So is the same join written as a LEFT JOIN to a
parenthesised pair (`k k2 left join (e join k on ...) on ...`). Both are
the LEFT JOIN of the preserved table to the pair. PostgreSQL joins the
pair first, because an outer join's minimum nullable side holds the inner
joins below it (`make_outerjoininfo`). The search then weighs hashing
either side. EXPLAIN draws the hash plan it keeps: a `Hash Right Join`
hashing the preserved table where that is cheaper, as when it is unique
on its key. Before, the chain was drawn as written, always hashing the
pair.

#### Fixed

- `explain (costs off) select e.b, k2.b from e join k on e.a = k.a right
  join k k2 on k2.a = e.a` (never analysed) is PostgreSQL 15.20's `Hash
  Right Join` of the pair over a Hash of `k k2`. In a randomised sweep, 11
  of 11 all-hash shapes now match, against 5 before. The rows and their
  order were the same before and after.
- Over analysed tables the chain is still drawn as written: the search
  does not run there until RP-1059 (1). RP-1063 stays open for that case.
- Filed RP-1114: under such a chain, a pair PostgreSQL joins with a Merge
  Join returns one key's rows in another order.

Fixes RP-1063, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a right join atop a never-analysed chain hashes its unique preserved relation])
