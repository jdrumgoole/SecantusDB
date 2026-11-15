### Rust MongoDB server: a `collMod` of a missing collection names `<db>.$cmd`

A `collMod` of a collection that does not exist is refused before it
acquires one, as mongod 8.2.11's is: its slow-query line names
`<db>.$cmd` and it writes no profile entry.

#### Fixed

- A `collMod` refused with `NamespaceNotFound` named the collection on its
  line and wrote a profile entry with `ok: 0`.

Fixes RM-584 (tests/backlog/test_rm_ts_ddl_lines.py::test_a_coll_mod_of_a_missing_collection_names_cmd)
