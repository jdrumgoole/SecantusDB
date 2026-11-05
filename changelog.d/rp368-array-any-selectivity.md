### Rust PG server: EXPLAIN estimates `= ANY (array column)` and analysed array operators

`EXPLAIN` of a scan filtered by `constant OP ANY / ALL (array_column)`, or
by an array column's `@>`, `<@` or `&&` over a table ANALYZE has measured,
printed zero costs and rows. It now prints PostgreSQL 15.19's estimate,
ported from `array_selfuncs.c`: `scalararraysel`'s containment path for the
element type's own `=` and `<>` (and its ten-element default for any other
operator), and `mcelem_array_selec` over the column's MCELEM and DECHIST
slots, with the null fraction applied.

#### Fixed

- `explain select * from ud where 1 = any(a)` prints `rows=7` and the
  `ScalarArrayOpExpr`'s cost; after `analyze`, `a @> '{1}'` prints the
  element's measured frequency, and `a <@ '{1,2,3}'` the
  distinct-element-count histogram's estimate.
- Fixes RP-368 (tests/backlog/test_rp_explain_costs.py::test_plan[a constant equal to ANY of an array column], tests/backlog/test_rp_explain_costs.py::test_plan[an analysed array containment])

#### Found

- RP-370: a cast constant (`1::bigint`) in a Filter deparses as `(1)::bigint`.
