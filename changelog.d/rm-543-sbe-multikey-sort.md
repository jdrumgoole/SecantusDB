### A `find` sorted on several fields explains as SBE under `queryFramework: "sbe"`

A `find` sorted on two or more top-level fields, under a query setting of
`queryFramework: "sbe"`, was explained in the classic shape. It now prints
and runs as mongod 8.2.11's slot program: a `sort` over a `project` of each
key's sort key, with the parallel-arrays check (`isArray`s joined by `||`
for two keys, a sum of `<=>`s for more).

#### Fixed

- Fixes RM-543 in part (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_sorted_find_is_mongods)
