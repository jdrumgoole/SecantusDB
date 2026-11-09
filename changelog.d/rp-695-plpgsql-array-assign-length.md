### Rust PG server: a PL/pgSQL assignment to a `varchar(n)[]` variable refuses an overlong element

A PL/pgSQL array variable silently kept a truncated element. `declare x
varchar(3)[]; begin x := array['abcdef'];` stored `{abc}`, where PostgreSQL
15.19 raises 22001 `value too long for type character varying(3)`. The same
held for a DEFAULT, a `'{abcdef}'` literal, a nested or bounded array, a
`text[]` value, `SELECT ... INTO`, a `char(3)[]` variable, and an array of a
domain over `varchar(3)`. A `bit(3)[]` variable took an element of another
length, where PostgreSQL raises 22026.

PostgreSQL coerces an array by an `ArrayCoerceExpr` whose element step is the
element type's length coercion, applied by the same assignment rule as a
scalar's. Since RP-687 a scalar variable was fitted this way, but an array
was still cast explicitly, which truncates every element. An array of (a
domain over) `char(n)`, `varchar(n)`, `bit(n)` or `varbit(n)` is now read at
its element's bare base type, each element is fitted to the modifier (its
nesting and lower bounds kept, a NULL left NULL), and only then is the array
cast to the declared type. A value already of the declared type and modifier,
such as `array['abcdef']::varchar(3)[]`, is still no coercion and keeps the
cast's `{abc}`, as on PostgreSQL.

#### Fixed

- Fixes RP-695 (tests/backlog/test_rp_found_rp687.py::test_array_variable_assignment_refuses_an_overlong_element)

#### Filed

- RP-710: an INSERT or UPDATE of a `varchar(n)[]`, `char(n)[]` or `bit(n)[]`
  column keeps an overlong element
  (tests/backlog/test_rp_found_rp695.py::test_array_column_assignment_fits_each_element).
- RP-711: a PL/pgSQL assignment to an array element, `x[1] := v`, is refused
  (tests/backlog/test_rp_found_rp695.py::test_assignment_to_an_array_element).
