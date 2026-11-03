### Rust PG server: EXPLAIN gates a column equated to two constants

`explain select * from e where a = 1 and a = 2` now prints PostgreSQL
15's plan: a `Result` with `One-Time Filter: false` over the Seq Scan,
which filters on `a = 1` alone and is priced for that clause.

#### Fixed

- A column's equalities to constants form one equivalence class, as
  `generate_base_implied_equalities_const` builds it. The scan keeps the
  first constant. Each other distinct constant folds to a `false` gate:
  one prints as `false`, several as `(false AND false)`. The gate sits
  directly over the scan, under any Sort, Limit or Aggregate. A repeated
  equality (`a = 1 and 1 = a`) is now one clause. Modelled for integer
  and text columns. `EXPLAIN ANALYZE` still runs and shows the statement
  as written.

Fixes RP-350 (tests/backlog/test_rp_explain_costs.py::test_plan[a column equal to two constants]).
