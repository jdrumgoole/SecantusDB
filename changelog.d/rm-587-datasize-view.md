### Rust MongoDB server: a `dataSize` of a view is refused

A `dataSize` of a view now answers CommandNotSupportedOnView (166), as
mongod 8.2.11 does, instead of `ok: 1` sizing nothing.

#### Fixed

- `dataSize` refuses a plain view, not only a time-series collection.
  Fixes RM-587 (tests/backlog/test_rm_view_ddl_refusals.py::test_a_data_size_of_a_view_is_refused)
