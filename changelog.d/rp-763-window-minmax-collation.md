### Rust PG server: a window `min` / `max` under a collation or over an enum picks in that order

A window `min` / `max` under a locale collation compared the values' bytes,
so `min(x collate "en-x-icu") over ()` over `a` and `B` answered `B` where
PostgreSQL answers `a`. Over an enum it compared the labels' text instead of
their sort order. The plain aggregates already picked in the right order.

#### Fixed

- `min` / `max` with OVER under a locale collation pick by the collation,
  with their window's ORDER BY keyed by it too, as `text_smaller` /
  `text_larger` do in PostgreSQL.
- `min` / `max` with OVER over an enum pick by the labels' positions and
  answer the enum type.

Fixes RP-763 (tests/backlog/test_rp_found_rp754.py::test_a_window_min_max_under_a_collation,
tests/backlog/test_rp_found_rp754.py::test_a_window_min_max_over_an_enum).

The differential check over a window `max` under `en-x-icu` orders its
outer query by `"C"`: ordered by the database's default collation, it
followed the reference server's locale and failed against CI's.
