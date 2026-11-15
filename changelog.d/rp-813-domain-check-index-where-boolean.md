### A domain's CHECK and a partial index's WHERE must be boolean

`create domain d as int check (value)` created the domain, and `create index on t (x) where x` over an integer column created the index. PostgreSQL coerces both to boolean, as it does a table's CHECK: the domain's is 42804 `argument of CHECK must be type boolean, not type integer` with no position (`domainAddConstraint` has no query text), and the index's is 42804 `argument of WHERE ...` at the predicate. The Rust PostgreSQL server now refuses both the same way, as well as `ALTER DOMAIN ... ADD CHECK` over a non-boolean expression.

#### Fixed

- Rust PG server: a domain's CHECK (CREATE / ALTER DOMAIN) and a partial index's WHERE are coerced to boolean, 42804 as on PostgreSQL 15.19.
  Fixes RP-813 (tests/backlog/test_rp_found_rp811.py::test_a_domain_check_and_an_index_predicate_are_coerced_to_boolean)
