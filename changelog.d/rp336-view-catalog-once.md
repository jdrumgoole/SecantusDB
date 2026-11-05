### Rust PG server: CREATE VIEW reads PostgreSQL's own catalog rows once per process

A view's `pg_rewrite` tree is built from a snapshot of the catalogs. Every
CREATE VIEW, CREATE MATERIALIZED VIEW and CREATE RULE used to re-read every
catalog row, PostgreSQL's own included. Now the `initdb` rows (oid below
16384) are read once per process and only the user's rows on each call. In a
debug build a CREATE VIEW drops from 1.3 s to 0.5 s.

#### Changed

- Part of RP-336, which stays open for the remaining cost
  (`tests/test_rust_pg_rule_trees.py::test_user_objects_made_after_a_first_view_reach_its_tree`).
