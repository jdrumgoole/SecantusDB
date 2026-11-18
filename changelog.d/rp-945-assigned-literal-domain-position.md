### A field's domain error in an assigned literal points at the literal

`insert into tct(c) values ('(0,x)')`, where the composite column's field
`a` is a domain whose CHECK the `0` fails, now reports the error at the
literal, as PostgreSQL does: it reads an untyped literal with the column
type's input while it analyses the statement, under the literal's
position. So does `update ... set c = '(0,x)'`, a literal written to a
domain-array column, and an `ARRAY['(0,x)']::ctp[]` element. Each had no
position.

#### Fixed

- `check_assigned_literal` reads an untyped literal assigned to an array or
  composite column as its cast to the column's type, raising a domain
  error there; `failing_array_element` places an `ARRAY[]` element's.
- `INSERT ... SELECT '(0,x)'` stays open as RP-945.
- Fixes RP-945 (tests/backlog/test_rp_found_rp552.py::test_assigned_literal_domain_error_position)
