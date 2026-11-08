### Rust PG server: a cast to a domain's array applies the domain's modifier

`select '{ab}'::d[]`, where `d` is a domain over `char(3)`, answered `{ab}`
where PostgreSQL 15.19 answers `{"ab "}`, and `'{abcd}'::d[]` answered
`{abcd}`, an element longer than the domain allows, where PostgreSQL
refuses it with 22001 `value too long for type character(3)`. The cast
checked each element against the domain's CHECKs but dropped the base
type's modifier.

#### Fixed

- A cast from text to a domain's array is `array_in`, which reads each
  element with `domain_in`: the base type's input function at the domain's
  modifier, then the domain's constraints, element by element and a NULL
  element included. So a short `char(3)` element is padded, a long one is
  22001, a `varchar(3)` one is 22001 too, a `numeric(5,2)` one rounds or
  is 22003, and a CHECK sees the padded value. A domain over a domain
  takes its innermost base's modifier.
- A cast from an array value (`array['abcd']::d[]`) casts each element
  explicitly, as an `ArrayCoerceExpr` does, which truncates: `{abc}`.
- The same holds per row for a cast over an expression of the row
  (`array[t]::d[]`, `('{' || t || '}')::d[]`).

Fixes RP-570
(tests/backlog/test_rp_found_rp559.py::test_cast_to_domain_array_applies_typmod),
with the regression test
`tests/backlog/test_rp_found_rp570.py::test_expression_cast_to_domain_array`
and the crate test `tests::a_cast_to_a_domain_array_coerces_each_element`.
Files RP-579 (a cast to an array with a type modifier, `'{abcd}'::char(3)[]`,
ignores the modifier), RP-580 (a cast of a bare column to such a domain
ignores its modifier), RP-581 (a domain array literal's input error has no
position) and RP-582 (a numeric overflow's 22003 has no DETAIL).
