### Rust PG server: ANY / ALL compare with an array's elements, and an array column meets their type

`x op ANY (array)` and `x op ALL (array)` compare `x` with each element of
the array, and an array's element type is never itself an array:
`ARRAY['{x,y}'::text[]]` is a two-dimensional `text[]`. The Rust PostgreSQL
server compared with the sub-arrays instead, so it answered queries
PostgreSQL refuses and refused ones it answers.

#### Fixed

- An array column against ANY / ALL of an array (`a = any
  (array['{x,y}'::text[]])`, `a = any ('{{x,y}}'::text[])`, `a = any
  (array[a])`, `a <> all (...)`), in a WHERE or a select list, over a
  table, a VALUES list or no FROM at all, is 42883 `operator does not
  exist: text[] = text`, as on PostgreSQL 15.19. It used to answer the rows
  `a = '{x,y}'` would. An untyped literal against an array column (`a = any
  ('{x}')`) is 42704 `could not find array type for data type text[]`, where
  it was 22P02.
- ANY / ALL walk every element of a multidimensional array in row-major
  order. `c = any (b)` over an `int[]` column that holds `{{1,2},{3,4}}` in
  some row was 42883 `int4 = int4[]` for the whole query, and `1 = any
  ('{{2},{1}}'::int[])` was false. A scalar against a nested array's element
  type is checked too (`1 = any (array['{x}'::text[]])` is `integer = text`).
- An uncorrelated `ANY` / `ALL` / `IN` subquery whose rows are arrays still
  compares each row whole (`b in (select b ...)`), and a vector array's
  elements stay vectors (`int2vector`, `oidvector`, RP-575).

Fixes RP-740 (tests/backlog/test_rp_found_rp722.py::test_an_array_column_against_any_of_an_array_of_arrays,
tests/backlog/test_rp_found_rp722.py::test_any_and_all_compare_with_the_arrays_elements)

#### Found

- RP-742: a scalar compared with an array value names `int4 = int4[]` where
  PostgreSQL names `integer = integer[]`
  (`tests/backlog/test_rp_found_rp722.py::test_a_scalar_against_an_array_value_names_its_types`).
