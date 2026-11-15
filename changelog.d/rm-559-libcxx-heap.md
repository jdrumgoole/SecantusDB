### Rust MongoDB server: on macOS, equal heap entries leave in libc++'s order

A macOS arm64 build of `secantusd-rs` now pops its priority queues (mongod's
`std::priority_queue`s) as
libc++'s `std::pop_heap` does, as it already sorts as libc++'s
`std::sort` (RM-526): each platform's mongod is the reference, and the
macOS build links libc++. Linux builds keep libstdc++'s heap.

#### Fixed

- A SORT_MERGE whose children's heads tie on the sort key, and a `$near`
  whose points tie on distance, hand back equal entries in macOS mongod's order on
  macOS (`find({a: {$in: [7, "b", 5, null]}}).sort({b: 1})` over `{a: 1,
  b: 1}` returns `[3, 14, 13, 17]` there, as mongod 8.2.11 does).

Fixes RM-559 (tests/backlog/test_rm_found_rm477.py::test_a_sort_merge_beside_a_null_scan_breaks_ties_as_mongod)
