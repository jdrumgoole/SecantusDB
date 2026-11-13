### Rust PG server: a nested loop into an index caches its inner scan in a Memoize, as PostgreSQL does

When a nested loop's inner side is an index scan parameterized by the outer
row, and the outer side brings few distinct keys, PostgreSQL puts a Memoize
over the scan: each key is scanned once and repeats are answered from the
cache. EXPLAIN on the Rust PostgreSQL server now plans that node the way
PostgreSQL 15.19 does. `get_memoize_path` decides whether a Memoize is
possible, `cost_memoize_rescan` prices it from the key's distinct count, and
the cheaper path wins. The node prints its Cache Key and Cache Mode, with or
without costs. Under ANALYZE it also prints its hits, misses and memory, and
the scan's loops are the misses. The comparison against the inner index may
now also be a call of a built-in function over the outer row, such as
`length(e.b) < k.a`. That comparison becomes the scan's Index Cond, and the
call's cost is added to the scan's startup.

#### Fixed

- `select * from r left join k on r.a < k.a` (`r.a` ten values, `k.a` the
  primary key) is PostgreSQL's `Nested Loop Left Join (cost=0.29..4256.67)`
  over `Memoize (cost=0.29..6.15)` keyed by `r.a`. Before, the Index Scan sat
  directly under the join, uncached, at `cost=0.28..9487.50`.
- The cache key is each moved clause's outer operand, in clause order and
  with repeats kept (`r.a, r.a`, `r8.a, r8.s`, `(r.a + 1)`). Its distinct
  count is `estimate_num_groups` over the Vars those operands read, so two
  Vars of one table are clamped to a tenth of its rows. There is no Memoize
  when `enable_memoize` is off, when the outer side has fewer than two rows,
  when a moved clause is not a two-operand operator, or when an operand's
  type has no hash operator class.
- A Memoize's entry size leaves out the cache key's width, as PostgreSQL 15
  does. The planner used to add that width, which is the PostgreSQL 16
  formula.
- `select * from e, k where length(e.b) < k.a` is priced as PostgreSQL
  prices it: the Index Cond `(a > length(e.b))`, the call's `procost` added
  to the scan's startup, and a Memoize keyed by `length(e.b)`. Before, the
  nested loop printed zeros. A call is taken only when it is a non-volatile
  built-in whose overloads all have the same `procost`.
- EXPLAIN ANALYZE of such a plan prints `Hits`, `Misses`, `Evictions`,
  `Overflows` and `Memory Usage`. The memory is counted as PostgreSQL's
  `EMPTY_ENTRY_MEMORY_BYTES` and `CACHE_TUPLE_BYTES` count it. The index
  scan's loops and its heap fetches come from the misses only. The JSON form
  prints the key as a single text and the counts as fields.
- The JSON form of EXPLAIN ANALYZE now prints a parameterized index scan's
  `"Rows Removed by Index Recheck": 0`, with or without a Memoize.

Fixes RP-916 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into an index under an outer side of few values])
Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into an index bounded by a call of the outer side])
