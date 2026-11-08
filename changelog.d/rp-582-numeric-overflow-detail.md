### Rust PG server: a numeric overflow's 22003 carries PostgreSQL's DETAIL

`select '123456'::numeric(5,2)` was 22003 `numeric field overflow` with
no DETAIL, where PostgreSQL 15.19's `apply_typmod` explains the overflow:
`A field with precision 5, scale 2 must round to an absolute value less
than 10^3.`

#### Fixed

- A value too wide for a `numeric(p,s)` modifier -- in a cast, an
  assignment to a column or a domain over the type -- is now 22003 with
  `apply_typmod`'s DETAIL, and an infinity with its `cannot hold an
  infinite value` DETAIL. The planner had built the text and dropped it.

Fixes RP-582 (tests/backlog/test_rp_found_rp570.py::test_numeric_overflow_detail).
