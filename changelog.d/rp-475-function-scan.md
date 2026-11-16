### EXPLAIN names the Function Scan of a user function in FROM

`explain select * from f()` over a user-defined function now prints the
`Function Scan on f` PostgreSQL runs, with its alias after it, instead of a
`Subquery Scan` over a `Values Scan` -- the rows the call was planned as.
Its price is still to come.

#### Fixed

- EXPLAIN of a user function in FROM prints `Function Scan on <function>`.
  Fixes part of RP-475 (tests/backlog/test_rp_from_function_scan.py::test_function_scan_plan)

#### Changed

- `explain_series_scan`'s return type is named, clearing a
  `clippy::type_complexity` error the current toolchain raises.
