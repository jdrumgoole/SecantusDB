### Rust PG server: a scalar subquery over a `char(n)` column keeps its blanks

`select (select c from t)` over a `char(3)` column holding `'xy'` now returns
`xy ` and `octet_length((select c from t))` is 3, as on PostgreSQL 15.19. The
subquery's value was cast to `bpchar` without the column's modifier, so the
value stored without its padding came back without it. It is now cast at the
column's modifier (`exprTypmod` of an EXPR sublink is its column's), as an
`ARRAY(select ...)` sublink has been since RP-656; a `char(n)[]` column's
elements are padded the same way, and an empty subquery's NULL is described
at the column's modifier.

#### Fixed

- A scalar subquery over a `char(n)` or `char(n)[]` column keeps the value's
  trailing blanks, and describes the column's modifier when it returns no
  row.

Fixes RP-666 (tests/backlog/test_rp_found_rp656.py::test_scalar_subquery_keeps_char_padding)
