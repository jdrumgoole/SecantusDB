### Rust PG server: a DROP TABLE of several tables is refused as one statement

A RESTRICT `DROP TABLE a, b` that something depends on is now refused as
PostgreSQL refuses it: one `cannot drop desired object(s) because other
objects depend on them`, covering every table named.

#### Fixed

- PostgreSQL drops the tables of one `DROP TABLE` in a single
  `performMultipleDeletions`, and `reportDependentObjects` names the object
  in the headline only when the statement drops one. The Rust PG server
  dropped each table on its own, so `drop table m564a, m564b` was refused
  as `cannot drop table m564a because other objects depend on it`, and the
  DETAIL named only the first table's dependants.
- A RESTRICT drop naming more than one table that exists now gathers every
  table's dependants before anything is dropped. The DETAIL reads them as
  PostgreSQL 15.19 does: the last table's dependants first, and an object
  two of them reach (`function k(b, a)`) named once, under the table named
  first. An `IF EXISTS` list with only one table present keeps that table's
  name in the headline, as on PostgreSQL.
- Fixes RP-564 (tests/backlog/test_rp_found_rp546.py::test_multi_table_drop_refusal_headline),
  with the DETAIL order pinned by
  tests/backlog/test_rp_found_rp546.py::test_multi_table_drop_detail_order.
- Filed RP-858: a CASCADE drop of several tables still sends one notice
  per table, where PostgreSQL sends one for the statement.
