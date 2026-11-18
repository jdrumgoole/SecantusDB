### `record_image` operators compare arrays of text and numeric

`row('{a}'::text[]) *= row('{a}'::text[])` used to be refused as not supported. The `record_image` operators now lay out an array of text, varchar, char, numeric or bytea the way PostgreSQL stores it, each element carrying the 4-byte length header `array_in` gives it, and compare those bytes, so an ordering such as `'{b}'` sorting after a 63-character element matches PostgreSQL 15.19.

#### Fixed

- `*=` .. `*>=` over a record field that is an array of text, varchar, bpchar, numeric or bytea.
  Trims RP-976 (tests/backlog/test_rp_found_rp590.py::test_record_image_over_arrays_of_varlena_elements)
