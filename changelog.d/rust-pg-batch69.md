### Rust PostgreSQL server: an UPDATE in its scan's order

An UPDATE applies its rows in the order its scan reads them, as
PostgreSQL 15.19 does, and RETURNING shows the new versions.

#### Fixed

- An UPDATE applies its rows in the order its scan reads them: heap order
  for a sequential scan, the index's order for an index scan. That is the
  order their new versions take their places, its row triggers fire and
  RETURNING lists them (RP-247). Before, it applied them in the store's
  order. Once a row had moved, every new version after the first stood one
  place apart from PostgreSQL's, and so did the statement's buffer counts.

  Fixes RP-247 (tests/backlog/test_rp_batch68.py::test_scenario[an UPDATE after a row moved places rows as PostgreSQL does]).
- An UPDATE's RETURNING shows each row's new version: its `ctid`, `xmin`
  and `cmin` as the write stamped them, where it showed the old version's
  (RP-248). That holds for a row a new primary key moves too.

  Fixes RP-248 (tests/backlog/test_rp_batch69.py::test_scenario[an UPDATE returns its rows in scan order with their new ctids]).
