### Rust MongoDB server: `$setWindowFields` returns tied documents in mongod's order

mongod runs `$setWindowFields` behind a `$sort` on its partition and `sortBy`. That sort uses the C++ library's `std::sort`, which is not stable, so documents whose keys tie come back reordered: 40 documents over three values of `k` start 39, 3, 27, 9 on macOS mongod 8.2.11 and 0, 21, 18, 24 on the Linux build. This server already ran that sort to hold it to its memory budget, but then dropped the result, so the window returned ties in the order it read them. The stage now takes its documents in the order that sort leaves them. That covers ties in a partition with or without a `sortBy`, descending keys, a computed partition, a window behind `$match` or `$addFields`, and a sort that spills to disk, whose ties come in the order its runs merge.

#### Fixed

- `$setWindowFields` orders tied documents as mongod 8.2's sorter does.
  Fixes RM-527 (tests/backlog/test_rm_found_2026_11_10_rm402.py::test_tied_order_of_find_leading_sort_and_window, tests/backlog/test_rm_found_2026_11_10_rm402.py::test_window_ties_in_each_shape_and_when_spilled; crate test `aggregate::tests::a_window_takes_ties_in_its_sorts_order`).

#### Known gaps

- A window behind an `$unwind` still returns ties in the order read. mongod moves the window's sort ahead of the `$unwind` and sorts the documents before they are unwound. Filed as RM-640.
