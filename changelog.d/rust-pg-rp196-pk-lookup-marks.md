### Rust PostgreSQL server: a primary-key lookup marks the dead index entries it passes, so updated tables' indexes stay their PostgreSQL size

When a PostgreSQL index scan finds that an entry's row version is dead to
every transaction, it marks the entry so a later insert can reuse the space
(`kill_prior_tuple`). The Rust PostgreSQL server marked them for the index
scans its planner priced, but not for a lookup of one row by its primary
key. So a table updated between rounds of key lookups grew an index page
that PostgreSQL 15.19 does not. A unique-key lookup is now an index scan
whenever PostgreSQL's planner would choose one, and it checks only the row
versions behind that one key. The planner's verdict is kept until the
table's page count or the catalog changes, so a lookup costs what it did
before.

Repeated `information_schema` queries also got faster: a domain CHECK that
reads only its value now keeps its verdict for integer and boolean values.

#### Fixed

- `secantus-pg` (`unique_lookup_scanned`, `btree_key_scanned`,
  `dead_index_tids_among`): a lookup by every key column of a unique plain
  B-tree marks the dead versions behind that key's entries. Fixes RP-196
  (`tests/backlog/test_rp_batch81.py::test_primary_key_lookups_mark_the_dead_versions`).

#### Changed

- `secantus-pgplan` (`domains::check_fails`): the verdict of a domain CHECK
  built only of operators, casts and constants is remembered for an
  integer, boolean or NULL value (RP-131).
