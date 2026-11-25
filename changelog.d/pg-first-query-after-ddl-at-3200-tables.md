### Rust PG server: the first statement after DDL no longer walks every table to republish the planner's catalog

The first catalog query after a `CREATE TABLE` grew by about 0.9 ms for
each thousand tables in the schema (debug build): 13.7 ms with no other
table against 16.4 ms with 3,200, where PostgreSQL 15.19 takes the same
time at any size. Each new catalog version republished the planner's
tables by walking every table definition. It copied every table's name
into a set, which a lookup that missed searched (every unqualified name is
looked for in the session's temp schema first). It also walked every table
to find the partitions, the inheriting, temporary and collation-declaring
tables, and the one or two tables a keyed `pg_attribute` read is about.

#### Fixed

- The shared table definitions are installed as they are, with whether
  they are in the order of their names (each one's `_id` is its name, and
  the `_id`s are sorted); a name is then found by a binary search, and a
  name in the temp schema is answered at once while the session holds no
  temp relation. That order, and the few partition, inheriting, temporary
  and collating tables, are kept with each committed catalog version and
  patched from the definitions a commit changed, as PostgreSQL rebuilds
  only the cache entries an invalidation names (`inval.c`). Now 13.1 ms
  against 14.3 ms at 3,200 tables; the growth that is left keeps RP-1073
  open.

Fixes RP-1073, in part (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_3200_tables_within_a_tenth)
