### A btree_gin index over time has its pages, and make_time takes a leap second

A GIN index built with `btree_gin` over a `time` column now has the pages
PostgreSQL gives it, so `pg_relation_size` and the other size functions
read its real size instead of 0. Its keys are ordered as `time_cmp` orders
them, by microseconds since midnight. `make_time` (and `make_timestamp`)
now accept 60 seconds, as PostgreSQL's `float_time_overflows` does.

#### Fixed

- `pg_relation_size` of a `btree_gin` index over `time` (or a GIN index
  over `time[]`) read 0.
  Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_btree_gin_time_index_has_its_pages)
- `make_time(12, 0, 60)` raised 22008 "time field value out of range";
  PostgreSQL returns `12:01:00`, rounding the seconds to microseconds
  before its range check and allowing the total to reach 24:00:00.
