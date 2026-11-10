### Rust PG server: `CREATE DOMAIN`'s COLLATE clause is kept

`create domain d as text collate "C"` gives the domain the collation it
names on PostgreSQL, and every column of the domain sorts and compares by
it. The Rust PostgreSQL server parsed the clause and then dropped it, so
every domain took its base type's collation and a column of the domain
sorted bytewise whatever the clause said. `pg_type.typcollation` and
`information_schema.domains.collation_name` reported the base type's
collation, and `collation for` answered `"default"`. A COLLATE over a type
with no collation, and a collation that does not exist, were accepted.
`ALTER TABLE ... ADD COLUMN` dropped its own COLLATE clause the same way.
Both are now kept, under the rule PostgreSQL uses: a column's collation
comes from its own COLLATE clause, otherwise from its type, and a domain's
comes from its COLLATE clause, otherwise from its base type.

#### Fixed

- `CREATE DOMAIN ... COLLATE` records the collation. The domain and its
  array type carry it in `pg_type.typcollation`, and
  `information_schema.domains` names it. A domain defined over it inherits
  it unless it declares its own.
- A column of a collated domain (from `CREATE TABLE`, `CREATE TABLE AS` or
  `ADD COLUMN`) sorts and compares by the domain's collation, reports it
  in `pg_attribute.attcollation`, and answers it from `collation for`. A
  COLLATE clause on the column itself still wins.
- `CREATE DOMAIN ... COLLATE` and `ADD COLUMN ... COLLATE` refuse a type
  with no collation (42804 `collations are not supported by type
  integer`) and a collation that does not exist (42704).
- `ALTER TABLE ... ADD COLUMN x text COLLATE "C"` keeps its collation
  instead of dropping it.
- `information_schema`'s `character_data` and `yes_or_no` domains carry
  `COLLATE "C"`, as `information_schema.sql` declares them.

Fixes RP-782 (tests/backlog/test_rp_found_rp769.py::test_create_domain_keeps_its_collate_clause, tests/backlog/test_rp_found_rp769.py::test_a_column_takes_its_domains_collate_clause)
