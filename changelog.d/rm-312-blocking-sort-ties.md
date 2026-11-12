### Rust MongoDB server: a blocking sort leaves equal documents where mongod's `std::sort` does

mongod sorts a `find`'s or a leading `$sort`'s documents with its sorter's
`std::sort`, which is not stable, so documents whose sort keys compare equal
come back in the C++ library's order, not the order they were read in. The
Rust MongoDB server kept them in the order it read them. It now sorts them
as libstdc++'s introsort does, the library of the Linux build that is
mongod's exemplar, over the order the plan hands them to its `SORT`.

#### Fixed

- `find().sort(...)` and an aggregation that starts with `$sort`, with a
  collation or without, return documents whose keys tie in Linux mongod's
  order. A sort with a limit keeps its heap's order, as before (section
  7.075); on macOS arm64, whose mongod links libc++, ties still come in the
  order read (RM-526). `$setWindowFields`'s own sort is RM-527.

Fixes RM-312 (tests/backlog/test_rm_found_2026_10_25.py::test_a_blocking_sort_leaves_ties_as_mongods_sort_does)
