### Rust PG server: writing an array column fits each element to the column's modifier

A `varchar(n)[]`, `char(n)[]`, `bit(n)[]`, `varbit(n)[]` or `numeric(p,s)[]`
column stored whatever array it was given. `insert into t (a) values
(array['abcdef'])` into a `varchar(3)[]` column wrote the six-character
element, where PostgreSQL 15.19 raises 22001 `value too long for type
character varying(3)`. A `numeric(4,1)[]` column kept `{1.25,2.35}` unrounded
and took `12345.0` without 22003, a `varbit(2)[]` column took `B'101'`, and
`array['abc   ']` kept its trailing blanks where PostgreSQL stores `{abc}`.

PostgreSQL coerces an assigned array by an `ArrayCoerceExpr` whose element
step is the element type's length coercion, applied by the assignment rule.
The fitting RP-695 built for PL/pgSQL now applies to every write of an array
column: an INSERT's VALUES or SELECT, a bound parameter, an UPDATE's constant
or per-row expression, ON CONFLICT DO UPDATE, and a subscripted `SET a[i] =
v` (whose new element is fitted to the element type at the column's
modifier). Nesting, NULL elements and lower bounds are kept.

#### Fixed

- Fixes RP-710 (tests/backlog/test_rp_found_rp695.py::test_array_column_assignment_fits_each_element),
  with every write path pinned by
  tests/backlog/test_rp_found_rp710.py::test_every_write_path_fits_an_array_columns_elements
  and tests/backlog/test_rp_found_rp710.py::test_a_bound_array_parameter_is_fitted_to_the_column.

#### Filed

- RP-718: COPY FROM ignores its column's modifier, scalar or array
  (tests/backlog/test_rp_found_rp710.py::test_copy_reads_a_field_at_its_columns_modifier).
- RP-719: a `char(n)[]` column's elements are stored unpadded
  (tests/backlog/test_rp_found_rp710.py::test_a_char_n_array_element_is_padded_to_its_width).
