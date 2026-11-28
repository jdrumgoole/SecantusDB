### `pg_get_viewdef` relabels a domain column a connection has not seen published yet

The Rust PostgreSQL server printed its views' definitions before it
published the catalog's domains, so the printer read the domains its thread
had published for an earlier catalog. A view over a domain made since then,
by another connection or before a new connection's first statement, printed
`WHERE (u.dd > 2)` where PostgreSQL 15.20 prints
`WHERE ((u.dd)::integer > 2)`, and kept that answer until the next DDL.

#### Fixed

- The view, index, trigger and statistics definitions printed when the
  planner's catalog is published are now printed after every table they
  read from it.
- A failed catalog read while publishing that catalog left its table empty
  and the publication recorded as current for the catalog version. Such a
  publication is now marked incomplete and read again by the next
  statement. Raising the read's error in the statement is still open, as
  RP-1231.
- Fixes RP-1231 in part (`tests/test_rust_pgserver_viewdef_domains.py::test_pg_get_viewdef_relabels_a_domain_another_connection_made`).
