### A LATERAL subquery reads a field of an earlier item's whole row

`(v).a` inside a `LATERAL` subquery, where `v` is a FROM item to its left,
now answers that item's column `v.a`, as PostgreSQL does and as an
expression subquery already did. It used to fail with `column "v" does not
exist`.

#### Fixed

- Rust PG server: `select l.* from (values (3)) v(a), lateral (select (v).a + 0 as k) l`
  answers `3`, and `(v).a` in the LATERAL subquery's WHERE is read the same way.
  Fixes RP-705 (tests/backlog/test_rp_found_rp699.py::test_lateral_whole_row_field)
