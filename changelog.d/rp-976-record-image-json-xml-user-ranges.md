### Rust PG server: `record_image` compares json, xml, user ranges and arrays of ranges or composites

The `record_image` operators (`*=`, `*<>`, `*<`, `*<=`, `*>`, `*>=`)
compare a record's fields by their binary images. A json or xml field, a
field of a range or multirange made by `CREATE TYPE ... AS RANGE`, and an
array of json, xml, ranges, multiranges or composites were refused with
0A000; each is now compared by the image PostgreSQL lays out, measured on
PostgreSQL 15.20.

#### Fixed

- A json or xml field is its text as it was written, so `row('{"a":1}'::json)
  *= row('{"a": 1}'::json)` is false.
- A user range's bounds are laid out as `range_serialize` writes them: a
  varlena bound of at most 126 bytes with a one-byte header, a longer one
  aligned with its 4-byte header, a fixed-length one at its subtype's
  alignment. Its multirange follows `make_multirange`.
- An array of ranges, multiranges or composites keeps each element's
  4-byte header.

Fixes RP-976 in part (tests/backlog/test_rp_found_rp590.py::test_record_image_over_json_and_xml,
tests/backlog/test_rp_found_rp590.py::test_record_image_over_user_ranges,
tests/backlog/test_rp_found_rp590.py::test_record_image_over_arrays_of_ranges_and_composites).
