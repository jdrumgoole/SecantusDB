### A `text` column is not written to a composite column by `INSERT ... SELECT`

`text` has no assignment cast to a composite type, so PostgreSQL refuses
`insert into t(c) select '(1,x)'::text` -- and the same bare literal under a
UNION, ORDER BY, DISTINCT, GROUP BY or a subquery in FROM, each of which
resolves it to `text` first -- with 42804 `column "c" is of type ctp but
expression is of type text`, before a row is read. The Rust PostgreSQL
server read the text as the composite instead. It now refuses it at the
source column, and an ORDER BY, DISTINCT or GROUP BY literal is typed
`text` for scalar targets too.

#### Fixed

- `INSERT ... SELECT` of a built-in type other than `record` into a
  composite column is 42804, as on PostgreSQL 15.19.
  Fixes RP-1044 (tests/backlog/test_rp_found_rp552.py::test_insert_select_text_into_composite)
