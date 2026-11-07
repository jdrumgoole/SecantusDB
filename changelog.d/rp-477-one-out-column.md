### Rust PG server: a function with one OUT parameter names its FROM column

A function with a single OUT parameter, or a `RETURNS TABLE` of one
column, called in FROM had its column named after the function or the
item's alias, so `select t from gto()` over `gto(out t int)` was 42703.
PostgreSQL's `chooseScalarFunctionAlias` names that column by the
parameter, ahead of the alias, and a composite result's column by its
type's field; only a scalar result's column takes the alias.

#### Fixed

- `select t from gto()`, `select g.t from gto() as g`, `select t from
  gtt()` and `select * from gto() g` now answer PostgreSQL 15.19's column
  `t`, as do an INOUT parameter's, a one-field composite result's (its
  field), and the same functions in a join, LATERAL, `ROWS FROM` and WITH
  ORDINALITY. A built-in in a join keeps its own name too:
  `json_array_elements(...) x` beside a table is `value`, where it was `x`.
- `f() WITH ORDINALITY g` over a scalar function names its column `g`, as
  without the ordinality; it was named by the function.
- The item's whole row (`select g from gto() g`, `select gto from gto()`,
  beside a table and under an outer join) is the function's scalar value,
  or its composite as a row (`select g from gtc() g` is `(8)`).
- The same whole row in a join: `select t.id, e from t, lateral
  jsonb_array_elements(t.j) as e` (and the implicit lateral of a comma
  join, `cross join lateral`, `left join lateral ... on true`, a function
  either side of a plain `join`) answers the element, as PostgreSQL 15.19
  does, where it was 42703 `column "e" does not exist`; `e.value` and a bare
  `value` still read the column. `lateral f(...) with ordinality e` and a
  LATERAL subquery `e` are a record, NULL where an outer join NULL-extends
  them, and a later function item can take the row as its argument
  (`jsonb_array_elements(e)`).
  Fixes RP-477 (tests/backlog/test_rp_function_bodies.py::test_one_out_parameter_names_the_column,
  tests/backlog/test_rp_function_bodies.py::test_joined_function_names_its_whole_row)
- Filed RP-483, found while working it: the same function called in a
  select list is named after its parameter (`select gtt()` is `t`, not
  `gtt`), and a field of its result is 0A000 rather than 42809.
