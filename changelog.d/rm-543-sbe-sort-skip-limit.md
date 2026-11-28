### A sorted `find` with a skip and a limit explains as SBE under `queryFramework: "sbe"`

A `find` sorted on top-level fields with both a `skip` and a `limit`, under
a `queryFramework: "sbe"` query setting, was explained in the classic
shape. It now prints and runs mongod 8.2.11's slot program: the sort's
top-k is `skip + limit`, a `let` of the two slots guarded by `typeMatch`,
and the skip a `limitskip none` over the sort.

#### Fixed

- Fixes RM-543 in part (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_sorted_find_is_mongods)
