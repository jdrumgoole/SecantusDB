### Rust PG server: an array of `int2vector` or `oidvector` holds one vector per element

Each element of an `int2vector[]` or `oidvector[]` is now ONE vector, as
on PostgreSQL 15.19, where `array_in` / `array_out` treat the element as
an opaque datum printed by `int2vectorout`. A vector is carried as an int
array, and inside an array that form read as one more dimension:
`'{"1 2"}'::int2vector[]` was `{{1,2}}`, and two vectors of different
lengths were refused as a ragged array. Inside an array a vector is now
carried as its canonical text, and it goes back to its int array when it
is taken out (a subscript, `unnest`, `= ANY`).

#### Fixed

- `'{"1 2"}'::int2vector[]`, `array['1 2'::int2vector]` and
  `array(select '1 2'::int2vector)` are `{"1 2"}`, and
  `array['1 2'::oidvector, '3']` is `{"1 2",3}`. They were `{{1,2}}`, and
  the last was 2202E `multidimensional arrays must have array expressions
  with matching dimensions`. `array[''::int2vector]` is `{""}`.
- `array_dims`, `array_ndims` and `cardinality` count vectors, and a
  subscript, `unnest`, `array_to_string`, `array_append`, `array_position`
  and `= ANY` each take or compare one vector: `(array['1 2'::int2vector,
  '3'])[1]` is `1 2` and equals `'1 2'::int2vector`, `unnest` gives two
  rows, `'3'::int2vector = any(array['1 2'::int2vector, '3'])` is true.
  Each was an error or the wrong answer.
- `to_json(array['1 2'::int2vector])` is `[[1,2]]`, each vector the JSON
  array `datum_to_json` makes of it.
- `array_agg` over vectors is `array_agg(anyarray)`: typed `int2vector` /
  `oidvector` (it was `int2vector[]`), 2202E `cannot accumulate arrays of
  different dimensionality` for vectors of different lengths (it answered
  a ragged array), and 42804 `array is not a valid int2vector` when its
  two-dimensional value is sent in text, as `int2vectorout` refuses it; in
  binary it is sent as PostgreSQL's `array_send` sends it.
- In binary format an `int2vector[]` or `oidvector[]` column is
  `array_send` over each vector's own send function, byte for byte as
  PostgreSQL 15.19; it went out as text.
- A vector element's input error is placed at its literal:
  `'{"1 x"}'::int2vector[]` is 22P02 at 8.

Fixes RP-575 (tests/backlog/test_rp_found_rp600.py::test_vector_array_elements,
tests/backlog/test_rp_found_rp600.py::test_vector_array_reads,
tests/backlog/test_rp_found_rp600.py::test_vector_array_binary)

#### Found

- RP-601: the array built-ins refuse an `int2vector` or `oidvector`
  (`cardinality('1 2'::int2vector)` is 42883 here, 2 on PostgreSQL)
  (tests/backlog/test_rp_found_rp575.py::test_array_builtins_over_a_vector).
- RP-602: an `int2vector` of two dimensions prints as text in a cast or
  `concat`, where PostgreSQL is 42804
  (tests/backlog/test_rp_found_rp575.py::test_two_dimensional_vector_text).
- RP-603: `oidvector`'s `<` and `>` compare element by element, not
  shorter first
  (tests/backlog/test_rp_found_rp575.py::test_oidvector_orders_by_length_first).
- RP-604: a parameter declared `int2vector[]` or `oidvector[]` has no type
  (tests/backlog/test_rp_found_rp575.py::test_vector_array_parameter).
- RP-605: an `int2vector` input's out-of-range error quotes only its number
  (tests/backlog/test_rp_found_rp575.py::test_int2vector_range_error_quotes_the_rest).
