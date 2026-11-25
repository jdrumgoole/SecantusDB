### A table the PostgreSQL server just created is found while another connection reads the catalog

An autocommit `CREATE TABLE` could be invisible to its own connection's next
statement. `'t'::regclass`, `to_regclass` and `pg_class` answered as if the
table did not exist (42P01), while `SELECT * FROM t` found it. The answer
stayed wrong until the next DDL statement. It needed only a second connection
reading the catalog at the same moment. With one connection creating tables
and another polling a catalog lookup, 26 of 300 tables went missing in ten
seconds on an empty database. On a large schema the reader could be the
server's own autovacuum worker, which is how the catalog-latency tests first
caught it. In PostgreSQL a committed table is visible to every later
statement, and now it is here too.

#### Fixed

- `secantus-pg`: an autocommit statement runs in a transaction of its own
  (`run_autocommit_write`). The catalog version that keys the server's
  process-wide catalog caches moved when the statement ran, before that
  transaction committed. A catalog read on another thread in that gap saw
  the catalog as it was before the change and was cached as current for the
  new version. The version now moves again once the commit lands, as
  PostgreSQL sends a transaction's invalidations at commit (`AtEOXact_Inval`),
  and as the implicit extended-protocol transaction and `COMMIT` already did.
  Fixes RP-1080
  (`tests/test_rust_pgserver_slice.py::test_an_autocommit_create_table_resolves_beside_a_catalog_reader`).
