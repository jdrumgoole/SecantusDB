### Rust PG server: ORDER BY a whole row sorts it, in either direction

With `fs` a table `(a int, c text)`, `select fs from fs order by fs desc`
came back in ascending order, and so did `group by row(a, c) order by row(a,
c) desc`. The sorts had no order for two records at all, so every row compared
equal and the sort kept its input order. The gap test inserted its rows in
ascending order, which made ASC look right and DESC look reversed.
PostgreSQL 15.19 orders a record by its btree opclass, `btrecordcmp`: field
by field, with a NULL field after every non-NULL one. A DESC sort reverses
that, so the all-NULL row comes first.

#### Fixed

- The executor's sorts order two records by `record_cmp`. That covers ORDER
  BY, a GROUP BY's ordered groups, DISTINCT, a set operation's ORDER BY, a
  window's ORDER BY and an aggregate's ORDER BY, over a table's whole row, a
  stored composite column or a `ROW(...)`, ascending and descending.

Fixes RP-535 (tests/backlog/test_rp_found_rp532.py::test_row_order_desc).
