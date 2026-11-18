### record_image operators compare multiranges

The binary-image record operators `*=`, `*<>`, `*<`, `*<=`, `*>` and `*>=`
now compare a field of a built-in multirange type the way PostgreSQL does:
by the bytes of the `MultirangeType` it builds, so `{[1.5,2)}` and
`{[1.50,2)}` differ and a longer multirange sorts by its range count first.
A field of a user-defined multirange is still refused by name.

#### Fixed

- Rust PG server: `record_image` comparisons over int4 / int8 / num / date /
  ts / tstz multirange fields were 0A000; they compare `make_multirange`'s
  image. Fixes RP-976, trimmed (tests/backlog/test_rp_found_rp590.py::test_record_image_over_multiranges)
