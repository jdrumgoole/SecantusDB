### A DROP TABLE CASCADE of several tables sends one notice

PostgreSQL drops every table a `DROP TABLE a, b CASCADE` names in one
`performMultipleDeletions` and reports what that takes with them in one
notice, `drop cascades to N other objects`, the last table's dependants
first and an object two of the tables reach named once. The Rust PostgreSQL
server dropped each table on its own and sent one notice per table, so a
client counting notices, or reading the DETAIL lines, saw a statement split
in two. It now sends the one notice PostgreSQL sends, from the same walk
that already refuses the RESTRICT form of the statement as one.

#### Fixed

- `DROP TABLE t1, t2, ... CASCADE` naming two or more existing tables sends
  one `drop cascades to` notice for the whole statement, before any table
  goes, in `reportDependentObjects` order (PostgreSQL 15.19).
  Fixes RP-858 (tests/backlog/test_rp_found_rp546.py::test_multi_table_drop_cascade_notice)
