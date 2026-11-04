### Rust PG server: a comma join keys on a domain column

`element_types`' row IN, written as a join to `(SELECT DISTINCT ...)` as
PostgreSQL's `convert_ANY_sublink_to_join` makes it, no longer builds
gigabytes of rows. The join to the subquery keys on its
`information_schema.sql_identifier` columns, which the comma join's
ordering had refused as a hash key, so the subquery was cross-joined to
six relations until the server was killed.

#### Fixed

- A WHERE equality between a domain column and a column of its base type
  family (`sql_identifier` = `name`, `character_data` = `text`) is a hash
  key of the join that brings the two together, so the join order follows
  it as PostgreSQL's equivalence classes do. The query answers 278 rows,
  as PostgreSQL 15.19 does in a fresh cluster.

Fixes RP-306 (tests/backlog/test_rp_semijoin_keys.py::test_element_types_semijoin_answers)
