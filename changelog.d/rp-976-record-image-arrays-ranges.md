### Record image comparisons over arrays and ranges

The `record_image` operators (`*=`, `*<`, ...) now compare an array field
by its `ArrayType` image and a range field by its `RangeType` image, as
PostgreSQL's `record_image_cmp` does with `memcmp`: dimensions, lower
bounds and the NULL bitmap decide before any element, and `[1.0,2]` and
`[1.00,2]` are different numranges. Before, both were refused.

#### Fixed

- Rust PG server: `record_image` comparisons over arrays of fixed-length
  elements (integers, oid, bool, `"char"`, floats, money, date, time, the
  timestamps, pg_lsn) and over `int4range`, `int8range`, `numrange`,
  `daterange`, `tsrange` and `tstzrange`. jsonb and arrays of varlena
  elements remain refused (RP-976 stays open for them).

Fixes RP-976 in part (tests/backlog/test_rp_found_rp590.py::test_record_image_over_arrays_and_ranges)
