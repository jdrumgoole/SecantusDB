### The Rust PostgreSQL server: index scans mark the dead entries they pass

An index scan now marks the B-tree entries whose row versions are dead to
every transaction, as PostgreSQL 15.19 does, and the next insert that finds
the page full deletes them instead of splitting it. So an index read between
updates keeps the index the size PostgreSQL keeps it.

#### Fixed

- `secantus-pg`: a read the planner prices as a B-tree index scan, and an
  ORDER BY with no WHERE that PostgreSQL reads in an index's order rather
  than sorting, mark LP_DEAD the entries they pass whose heap versions are
  all dead (`kill_prior_tuple`, `_bt_killitems`). 54 of 54 shapes of reads
  between updates (ordered reads, ranges, `IN` lists, at 500 to 6,000
  rows) keep the primary key at PostgreSQL's page counts, against 48
  before. Part of RP-196
  (`tests/backlog/test_rp_batch71.py::test_scenario[an index scan marks the dead versions it passes]`).
- `secantus-pgplan`: `costsize::index_order_beats_sort` weighs an index
  scan in an ORDER BY's order against the cheapest scan under a Sort, as
  `create_ordered_paths` does.

RP-196 stays open for a read that returns one row, which is not priced, so
a primary-key lookup marks nothing
(`tests/backlog/test_rp_batch81.py::test_primary_key_lookups_mark_the_dead_versions`).

#### Changed

- The psycopg gauge keeps the server's output in
  `.validation/psycopg-raw-<server>.server.log`, and the Windows job uploads
  it with the raw report. It was discarded, and RP-268 (an intermittent
  40001 seen once in that job) needs it.
