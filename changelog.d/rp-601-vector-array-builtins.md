### Rust PG server: the array built-ins take an `int2vector` or `oidvector`

`int2vector` and `oidvector` are true array types to PostgreSQL 15.19:
their `typsubscript` is the array subscript handler and their elements are
`int2` and `oid`. So `cardinality(indkey)`, `array_dims(proargtypes)` and
`array_to_string(indkey, ',')` work there, subscripted from 0:
`array_dims('1 2'::int2vector)` is `[0:1]`. Here every array built-in
refused a vector as 42883 `function ... does not exist`, because the
polymorphic `anyarray` accepted only a type spelled `elem[]`. Accepting the
vectors was not enough on its own. A vector reached the built-ins with a
lower bound of 1, so `array_position` was off by one. A vector column took
a per-row shortcut that never saw its type. And `'1 2'::int2vector ||
'3'::int2vector` was `{1,2,3}` where PostgreSQL gives `[0:2]={1,2,3}`.

#### Fixed

- The overload resolution now treats a vector as an array type whose
  element is `int2` or `oid`, as `get_element_type` does. `anyarray` and
  `anycompatiblearray` accept one, `anynonarray` refuses one, and
  `anyelement` resolves to the element. The type-category table now lists
  `int2vector` and `oidvector` under their own names.
- A vector passed where an array built-in declares `anyarray` (or
  `anycompatiblearray`) arrives as its element's array with a lower bound
  of 0, as the cast `'1 2'::int2vector::int2[]` already gave it. This
  applies to a literal, an expression and a column (`pg_index.indkey`,
  `pg_proc.proargtypes`). An array built-in over a vector column now takes
  the general path, which knows the column's type.
- `||` with a vector operand concatenates its element's array, lower bound
  0, typed `smallint[]` / `oid[]`. `array_append` / `array_cat` /
  `array_prepend` over a vector are typed the same way.

Fixes RP-601 (tests/backlog/test_rp_found_rp575.py::test_array_builtins_over_a_vector).
The crate regression test is `secantus-pgplan`'s
`tests::a_vector_is_an_array_to_the_polymorphic_resolution`.
