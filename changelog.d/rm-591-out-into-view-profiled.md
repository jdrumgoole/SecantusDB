### An `$out` into a view is profiled under the view

mongod 8.2.11 refuses an `aggregate` whose `$out` writes to a view (166),
and its `Slow query` line and profile entry both name the view. The Rust
MongoDB server's line named the collection the pipeline read, and it wrote
no profile entry.

#### Fixed

- The refused `$out` is profiled, and its line and entry name the view.
  Fixes RM-591 (tests/backlog/test_rm_view_namespaces.py::test_an_out_into_a_view_names_the_view)
