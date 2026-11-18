### record_image compares arrays of interval, uuid and other fixed-length by-reference types

The `record_image` operators (`*=`, `*<` and the rest) now compare a record
field holding an array of interval, uuid, timetz, name, macaddr, macaddr8 or
tid, as PostgreSQL does: each element is laid out as its full binary image at
its type's alignment and the arrays are compared byte by byte. Before, these
were refused with 0A000.

#### Fixed

- Rust PG server: `record_image` comparisons over arrays of fixed-length
  by-reference elements. Fixes RP-976, in part
  (tests/backlog/test_rp_found_rp590.py::test_record_image_over_arrays_of_fixed_length_by_reference_elements).
