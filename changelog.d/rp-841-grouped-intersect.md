### An INTERSECT over an aggregate hashes the smaller input first

`select max(x) from t group by k intersect select 1.000` returned the
aggregate's spelling of the shared value where PostgreSQL returns the
constant's: PostgreSQL makes the input with fewer estimated groups the
HashSetOp's first, and an aggregate's groups are its rows. The server now
estimates an aggregate over one table, with or without a GROUP BY of plain
columns, the same way.

#### Fixed

- An INTERSECT whose other input is an aggregate over one table orders
  its inputs by PostgreSQL's group estimates.
  Fixes RP-841 in part (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_grouped_input_puts_the_smaller_input_first)
