### Rust PG server: an ARRAY subquery is typed by its column, not by its values

An `ARRAY(...)` subquery of a built-in type now goes out as the array of
the subquery column's type, as PostgreSQL 15.19's `transformSubLink` types
it (`get_promoted_array_type`). Before, its value was a list with no
declared type, typed off its elements: a `smallint` subquery came out
`integer[]`, an empty one `integer[]` whatever its column, and several
element types lost their values.

#### Fixed

- `select array(select 1::int2)` is described as `smallint[]` (1005),
  `array(select 'a'::varchar(3))` as `character varying[]`,
  `array(select 1.5::float4)` as `real[]`, `array(select 'x'::name)` as
  `name[]`, and the same for `date`, `time`, `uuid`, `jsonb`, `json`,
  `"char"`, `oid`, `xid`, `inet`, `int4range`, a `char(n)` and a catalog
  column such as `pg_class.relname`. They were `integer[]`, `text[]` or
  `double precision[]`.
- An empty one is typed by its column too: `array(select 1::int2 where
  false)` is `smallint[]` and `array(select 'x' where false)` `text[]`;
  both were `integer[]`.
- A `timestamp`, `timestamptz`, `interval`, `regclass` or `regtype`
  element read back NULL, and a `pg_lsn` one as the number it is carried
  as; each now reads back as itself.
- `pg_typeof` of an ARRAY subquery names its type; it was 42P18 `could not
  determine data type of parameter $1` for every built-in element type.

Fixes RP-600 (tests/backlog/test_rp_found_rp554.py::test_array_sublink_element_type)

#### Found

- RP-575: an array of `int2vector` or `oidvector` is a two-dimensional int
  array (`'{"1 2"}'::int2vector[]` is `{{1,2}}` here, `{"1 2"}` on
  PostgreSQL) (tests/backlog/test_rp_found_rp600.py::test_vector_array_elements).
