### Rust MongoDB server: an `explain` of a write is not profiled

An `explain` of an `update`, a `delete` or a `findAndModify` no longer
writes a profile entry, as on mongod 8.2.11, which profiles none at any
verbosity, whether the explain answers or is refused. Its slow-query line
already named `<db>.$cmd`; an `explain` of a read, an `$out` or a
`mapReduce` is still profiled under its collection.

#### Fixed

- The profiler wrote an entry for an `explain` of a write, named for the
  collection.
  Fixes RM-486 (tests/backlog/test_rm_view_namespaces.py::test_an_explain_of_a_write_is_not_profiled)
