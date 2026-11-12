### EXPLAIN's index-scan and nested-loop costs round as PostgreSQL's do

PostgreSQL's planner adds up its costs in C, and on arm64 the compiler fuses
each `cost += a * b` into one multiply-add, which rounds once instead of
twice. That moves the last bit of a cost. Usually the change is too small to
see in the two decimals EXPLAIN prints, but a total that lands on a half cent
can round either way. `select * from k left join k k2 on k.a < k2.a and k.a
> 990 where k.a > 900` totals exactly 1076.525 in theory. PostgreSQL 15.19
lands a hair above that and prints 1076.53. The Rust PostgreSQL server
summed the same terms unfused, landed a hair below and printed 1076.52.

The server now fuses the same multiply-adds PostgreSQL's build does. In an
index scan's cost these are the index tuples' CPU, the tree descent and the
heap I/O between its correlated and uncorrelated bounds. In a nested loop's
cost they are the inner side's rescans and the CPU of each pair it checks.

#### Fixed

- `EXPLAIN` of a nested loop over two index scans printed a total one cent
  under PostgreSQL 15.19's when the sum fell on a half cent.
  Fixes part of RP-354
  (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join of two index scans with a one-sided ON conjunct],
  crates/secantus-pgplan/src/costsize.rs::tests::a_parameterized_index_scan_fuses_its_multiply_adds).
  RP-354 stays open for the nested loops still printed at zero.
- Filed RP-912 (a binary-coercible cast is kept in a plan and its scan is
  not priced) and RP-913 (an IN list under a join prints as an `ARRAY[...]`
  constructor).
