### Rust MongoDB server: an int parameter bounds a double before truncating it

`setParameter` now refuses `2147483647.9` and `-2147483648.5` for an int
parameter with mongod 8.2.11's "Out of bounds coercing to integral value",
instead of truncating them into range and storing them.

#### Fixed

- Fixes RM-541 (tests/backlog/test_rm_found_2026_11_10_rm417.py::test_an_int_parameter_bounds_the_double_before_truncating)
