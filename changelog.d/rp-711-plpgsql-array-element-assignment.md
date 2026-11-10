### Rust PG server: PL/pgSQL assigns to an array element or slice

`x[i] := v` and `x[i:j] := v` in a PL/pgSQL function or DO block now work
as they do on PostgreSQL 15. Before this, the Rust PostgreSQL server cast
the value to the whole array's type, so `declare x int[]; begin x[1] := 5;`
failed with 22P02 `cannot cast int4 to int4[]`, and the usual append idiom
`x[array_length(x, 1) + 1] := v` failed the same way. The assignment is a
port of PostgreSQL's `array_set_element` / `array_set_slice`, and
`UPDATE ... SET a[...] = v` now uses the same port.

#### Fixed

- A PL/pgSQL assignment to an array element or slice. Each subscript is
  coerced to `int4` by the assignment rule (`x[true]` and `x['1'::text]` are
  42804) and checked before the value (a NULL subscript is 22004). The value
  is coerced to the element type, or for a slice to the array type, so an
  overlong element of a `varchar(3)[]` is 22001. A one-dimensional array
  grows to meet a subscript past either end, and its bounds move
  (`x[0] := 9` over `{1,2}` is `[0:2]={9,1,2}`). Assigning into a NULL array
  builds one at the subscripts given (`x[1][2] := 5` is `[1:1][2:2]={{5}}`).
  A domain over an array is checked after the assignment, and a `record`
  variable's array field takes an element (`r.a[2] := 5`). A jsonb
  subscript is refused by name (0A000).
- `UPDATE ... SET a[...] = v` now answers as PostgreSQL does in the cases
  the code it replaces got wrong. A subscript past a multidimensional
  array's bounds is 2202E `array subscript out of range`; before, the array
  was extended. A slice whose source is NULL leaves the array as it was;
  before, it was an error. A slice of a multidimensional array is assigned;
  before, it was refused. A NULL subscript is 22004, and an array grown past
  the size limit is 54000.

Fixes RP-711 (tests/backlog/test_rp_found_rp695.py::test_assignment_to_an_array_element, tests/backlog/test_rp_found_rp695.py::test_update_of_an_array_element_or_slice)

Files RP-771 (an array literal whose upper bound passes `int4` is accepted)
and RP-772 (jsonb subscripting is refused, with a 42804 that says jsonb does
not support it), found while probing.
