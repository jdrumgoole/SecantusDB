### Rust PG server: CREATE TABLE AS makes bit and bit varying columns

`create table t as select B'101'::bit(3) b` makes a `bit(3)` column, as it does
on PostgreSQL, where the Rust PostgreSQL server refused it as an unsupported
type. A `bit varying(n)` column and arrays of either type work too. An untyped
bit literal such as `B'1101'` makes a `"bit"` column with no modifier, which
takes a bit string of any length rather than being held to `bit(1)`.

`format_type` given a modifier of -1 now names `bit` and `bpchar` as
PostgreSQL's `format_type_extended` does, `"bit"` and `bpchar`, so the name
does not read back as `bit(1)` or `character(1)`.

#### Fixed

- `secantus-pg`: `internal_type_name` names the bit types (oids 1560, 1561,
  1562, 1563), so CREATE TABLE AS can make a column of them.
- `secantus-pgplan`: assigning to a `bit` column with no modifier (typmod -1)
  checks no length, as PostgreSQL's `bit()` does. A bare `bit` in DDL is still
  `bit(1)`, as the grammar gives it.
- `secantus-pgplan`: `format_type(1560, -1)` is `"bit"` (`"bit"[]` for the
  array) and `format_type(1042, -1)` is `bpchar`; `NULL` keeps `bit` and
  `character`.

Fixes RP-494 (tests/backlog/test_rp_ctas_subquery.py::test_ctas_over_a_bit_column)
