### Rust MongoDB server: a spilled sort of index keys under a collation writes collation keys

A `find` whose SORT of index keys spills under a collation now writes each
member's sort key as its collation key, as mongod 8.2 does, so
`spilledBytes` matches mongod's (157780 for 2000 strings under
`{locale: "fr"}`, where it was 149780).

#### Fixed

- The spilled sort key holds each string's collation key, written as the
  string `Value` mongod's sorter writes.

Fixes RM-555 (tests/backlog/test_rm_found_rm475.py::test_a_spilled_sort_of_index_keys_writes_collation_keys)
