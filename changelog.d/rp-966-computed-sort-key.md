### EXPLAIN prices and prints a Sort on a computed key

`EXPLAIN` of a query over one table ordered by an operator expression over
its returned columns, such as `select a from k order by a + 1`, now carries
PostgreSQL's costs and widths rather than zeros. The scan computes the key
as an extra hidden column, so the scan and the Sort are both wider and the
scan is costlier per row, as in PostgreSQL. Under `EXPLAIN VERBOSE` the scan
lists that hidden column in its `Output`, the Sort prints it parenthesised,
and its `Sort Key` is qualified: `Sort Key: ((k.a + 1))`.

#### Fixed

- A Sort over one relation's scan on a computed operator key is priced
  (`cpu_operator_cost` per operator per row, the key's type width), and
  `EXPLAIN VERBOSE` prints its resjunk entry. RP-966 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a sort on a computed key]);
  RP-1035 and RP-1036 filed.
