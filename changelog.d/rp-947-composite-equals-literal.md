### A composite column compared with a literal reads it as a record

`select * from tct where c = '(0,x)'`, `c` a composite column, is now 0A000
`input of anonymous composite types is not implemented` at the literal
whatever the literal holds, as on PostgreSQL: there is no `ctp = ctp`
operator, so `=` is `record = record` and the literal an anonymous record.
The server first read the literal as the column's type, as it does beside
an enum, so a field failing its domain was 23514 instead.

#### Fixed

- The analysis check that reads an untyped literal beside an enum skips a
  composite, which the user-type table also lists.
- Fixes RP-947 (tests/backlog/test_rp_found_rp552.py::test_composite_column_equals_untyped_literal)
