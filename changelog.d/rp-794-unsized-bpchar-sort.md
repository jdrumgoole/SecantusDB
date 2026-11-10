### Rust PG server: an ORDER BY over an unsized `bpchar` column ignores trailing blanks

A sort key over a `bpchar` column now orders the way `bpcharcmp` does,
comparing the values without their trailing blanks. An unsized `bpchar`
column (from a VALUES list or a set operation whose members' `char(n)`
widths differ) keeps its cells' blanks. The sort used to read them, so
`'a  '` came after `E'a\t'`, because a blank (0x20) sorts after a tab (0x09).
The same comparison drives a window's ORDER BY, its peer groups and an
aggregate's ORDER BY, so all of those now agree with PostgreSQL 15.19.

#### Fixed

- `select f from (values ('a  '::char(3)), (E'a\t')) s(f) order by f`
  returns `a  ` before `a<tab>`, with DESC reversed, and the same over a
  UNION ALL of a `char(3)` and a text literal.
- `row_number()` / `rank()` over `(order by f)` on such a column number the
  rows in PostgreSQL's order, and `'a  '` and `'a '` are peers.

Fixes RP-794 (tests/backlog/test_rp_found_rp775.py::test_an_unsized_bpchar_column_sorts_without_its_blanks)
