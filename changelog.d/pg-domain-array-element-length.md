### An array of a `varchar(n)` domain refuses an overlong element

The Rust PostgreSQL server stored a value too long for its element type in
a column that is an array of a domain over `varchar(n)` or `char(n)`. With
`create domain vd as varchar(3)` and a `vd[]` column, `array['abcd']`, the
same array from an `INSERT ... SELECT`, and the untyped literal `'{abcd}'`
were all stored, where PostgreSQL 15.20 refuses each one with 22001 `value
too long for type character varying(3)`. They are refused now, and the
literal's error is reported at the literal, as PostgreSQL reports it.

#### Fixed

- `secantus-pgplan`: an assignment to an array of a domain fits each element
  to the domain's base type at the domain's own modifier, as the
  `ArrayCoerceExpr`'s `coerce_to_domain` step does: an overlong element is
  22001 and trailing blanks past the length are dropped (`'ab  '` is stored
  as `'ab '`). INSERT, INSERT ... SELECT and UPDATE share the step.
- `secantus-pgplan`: an untyped literal assigned to such a column is read by
  `array_in` through `domain_in` at the domain's modifier, so its 22001
  carries the literal's position (29 in `insert into t48v(x) values
  ('{abcd}')`).

Fixes RP-1084 (tests/backlog/test_rp_found_rp1048.py::test_domain_array_element_length)
