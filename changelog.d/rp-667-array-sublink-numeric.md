### An ARRAY subquery over a sized numeric column describes its modifier

`select array(select n from t)` over a `numeric(5,2)` column now describes
`numeric[]` with modifier 327686, as PostgreSQL's `exprTypmod` gives an
ARRAY sublink its column's modifier, instead of -1.

#### Fixed

- An `ARRAY(select ...)` over a `numeric(p,s)` column described no
  modifier. Fixes RP-667 (tests/backlog/test_rp_found_rp656.py::test_array_sublink_of_numeric_describes_its_modifier)
