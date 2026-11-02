### Rust PG server: EXPLAIN prices a Unique over a Sort (RP-320)

`explain select distinct a from e order by a` now prints PostgreSQL's
estimate on its Unique, the Sort beneath it and the Seq Scan beneath that,
instead of zeros.

#### Fixed

- A SELECT DISTINCT whose ORDER BY is its distinct columns, over one user
  table with no index, prices its Unique as `create_upper_unique_path`
  does: the Sort's startup, a comparison of each column of every sorted row,
  and `estimate_num_groups`' rows. The estimate is printed only where that
  path is cheaper than the HashAggregate PostgreSQL also weighs (with the
  ORDER BY's Sort above it); where the HashAggregate wins, the Unique still
  prints zeros.

Fixes RP-320 (tests/backlog/test_rp_explain_costs.py::test_plan[unique])
