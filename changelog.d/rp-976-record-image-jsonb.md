### Image comparisons of records reach jsonb

The `record_image` operators (`*=`, `*<` and the rest) now compare a jsonb
field the way PostgreSQL does, by the bytes of its stored `JsonbContainer`:
object keys sorted by length then bytes, numerics padded to int alignment
and kept with their display scale, so `'1'::jsonb` and `'1.0'::jsonb` are
different images. Arrays of jsonb, inet and cidr are compared too. Each of
these was refused as unsupported before.

#### Fixed

- `record_image` comparisons over jsonb fields and over arrays of jsonb,
  inet and cidr returned 0A000 (Rust PG server).
  Fixes RP-976, in part (tests/backlog/test_rp_found_rp590.py::test_record_image_over_jsonb,
  tests/backlog/test_rp_found_rp590.py::test_record_image_over_arrays_of_jsonb_and_inet).
