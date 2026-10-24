### Rust PostgreSQL server: column statistics over ANALYZE's sample

A table past ANALYZE's sample has `pg_statistic` rows, and an inheritance
tree past it has extended statistics, both from the sample PostgreSQL
15.19 draws.

#### Fixed

- A table of more rows than ANALYZE samples (300 times
  `default_statistics_target`) has `pg_statistic` rows, so `pg_stats`
  lists its columns, where it listed none. They are computed from the
  sample `acquire_sample_rows` draws, as `compute_scalar_stats` computes
  them:
  - the distinct count is estimated for the whole table, and scaled by its
    rows past a tenth of it;
  - the most common values are those `analyze_mcv_list` keeps. A value is
    dropped while it is not significantly more common than the values left
    out of the list would be, unless every value was seen twice or more
    and they all fit;
  - `pg_class.reltuples` is the table's rows as the sample estimates them.
- An inheritance tree past the sample has extended statistics. Each member
  is sampled in proportion to its blocks and its rows estimated from its
  own sample, as `acquire_inherited_sample_rows` does.

  Fixes RP-099 (tests/backlog/test_rp_batch50.py::test_scenario[a table past ANALYZE's sample has column statistics],
  tests/backlog/test_rp_batch50.py::test_scenario[an inheritance tree past ANALYZE's sample has extended statistics]).
