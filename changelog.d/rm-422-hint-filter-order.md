### Rust MongoDB server: a hint to an index that bounds nothing keeps the FETCH filter's canonical order

`explain` of a find hinted to an index that no predicate bounds now lists the
FETCH filter in the canonical order, as mongod 8.2.1 does, instead of by path.

#### Fixed

- Fixes RM-422 (tests/backlog/test_rm_found_2026_11_08_rm331.py::test_a_hint_that_bounds_nothing_keeps_the_canonical_filter_order)
