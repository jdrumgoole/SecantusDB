### Rust PG server: a partition key keeps its COLLATE and operator class

A partition key's `COLLATE` and operator class are now resolved and kept
the way PostgreSQL 15's `ComputePartitionAttrs` does: checked when the
table is created, stored per key, served as `pg_partitioned_table`'s
`partcollation` and `partclass`, and printed by `pg_get_partkeydef` (and
psql's `\d+`) where they are not the key's own.

#### Fixed

- `partition by range ((x::text) collate "C")` printed `RANGE
  (((x)::text))`; it now prints `RANGE (((x)::text) COLLATE "C")`, and
  `((x::text) text_pattern_ops)` prints `RANGE (((x)::text)
  text_pattern_ops)`. A COLLATE that is the key's own collation is not
  printed (`upper(t) collate "C"` over a `"C"` column is `RANGE
  (upper(t))`), and neither is the type's default class.
- A COLLATE on a type without collations (42804), a collation or operator
  class that does not exist (42704), a class that does not take the key's
  type (42804), a key type with no default btree or hash class (42704,
  with PostgreSQL's hint) and an expression whose implicit collations
  conflict (42P16 `no collation was derived for partition key column 1`)
  are refused at `CREATE TABLE`, as on PostgreSQL. All of them were
  accepted.
- A top-level COLLATE in a key expression is stripped, as PostgreSQL
  strips it: `((t collate "C"))` is the column key `t COLLATE "C"`, with
  its `partattrs` entry the column's number rather than 0.
- `pg_partitioned_table` has the `partclass` and `partcollation` columns.

Fixes RP-1091 (tests/backlog/test_rp_found_rp860.py::test_pg_get_partkeydef_keeps_a_key_collation_and_opclass,
tests/backlog/test_rp_found_rp1091.py::test_a_partition_key_resolves_its_collation_and_opclass)
