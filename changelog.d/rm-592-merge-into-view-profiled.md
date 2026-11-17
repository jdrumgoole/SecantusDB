### A `$merge` into a view is profiled

The Rust MongoDB server refuses an `aggregate` whose `$merge` writes to a
view (166), as mongod does, but wrote no profile entry for it. mongod 8.2.11
profiles the refusal under the collection the pipeline reads; so does this
server now.

#### Fixed

- A refused `$merge` into a view writes a profile entry naming the source
  collection (`ok: 0`).

Fixes RM-592 (tests/backlog/test_rm_view_namespaces.py::test_a_merge_into_a_view_is_profiled)
