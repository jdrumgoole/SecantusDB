### Rust PG server: a simple CASE and NULLIF over a `char(n)` compare as `=` does

A simple CASE's `test = label` and NULLIF's `a = b` are each the `=`
operator their two types resolve to. Against a `varchar`, an untyped
literal or another `char(n)` that is `bpchareq`, which ignores the
trailing blanks on both sides; against `text` it is `texteq` over the
`char(n)` side's text. This server compared the `char(n)` side with its
padding, so `case c when v` over a `char(3)` `y` and a `varchar` `y` took
the ELSE branch and `nullif(v, c)` returned `y`.

#### Fixed

- `case c when v`, `case t when c`, `nullif(v, c)` and `nullif(t, c)`
  over a `char(n)` and a `varchar` or `text` operand answer as on
  PostgreSQL 15.19, in the select list and in a WHERE, as do two
  `char(n)` operands of different widths.

Fixes RP-737 (tests/backlog/test_rp_found_rp726.py::test_case_and_nullif_compare_by_type, tests/backlog/test_rp_found_rp737.py::test_case_and_nullif_compare_as_equals_does)

Filed while working it: RP-741 (NULLIF of a `char(n)` against an untyped
literal or NULL is typed `text` and drops its padding).
