### EXPLAIN prints the Filter of a WHERE over a user function in FROM

`EXPLAIN SELECT * FROM f() WHERE f > 1` over a user function now shows the
`Filter` line PostgreSQL prints under its `Function Scan`, deparsed against
the function's result columns, and prices it the way PostgreSQL does: each
operator in the qual costs `cpu_operator_cost` per row, and the qual's
selectivity scales the estimated rows.

#### Fixed

- EXPLAIN of a user function scan with a WHERE printed no Filter and kept
  every row in its estimate.
  Fixes RP-475 (tests/backlog/test_rp_from_function_scan.py::test_function_scan_filter)
