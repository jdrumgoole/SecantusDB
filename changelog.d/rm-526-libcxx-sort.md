### Rust MongoDB server: on macOS, tied sort keys come in libc++'s `std::sort` order

mongod's sorter sorts what it holds with `std::sort`, which is not stable, so documents whose sort keys compare equal come back in whatever order the C++ library's algorithm leaves them. The Linux build links libstdc++, which this server has followed since RM-402. On macOS it kept those documents in the order it read them, while mongod 8.2.11 on macOS, linked against libc++, reorders them. The macOS build now sorts with a step-for-step copy of libc++'s introsort, including its heap-sort fallback. This covers `find`, a leading `$sort`, a `$sort` after another stage, and the runs a spilling sort writes. Forty documents over three values of `k` now come back 39, 3, 27, 9, ... as they do from mongod. The spill files of tied sorts now store mongod's bytes: 51260 for `find`'s `{k: -1}` and 51240 for `{k: 1}`.

#### Fixed

- `crates/secantus-core/src/stl/libcxx.rs`: libc++'s `std::sort` (LLVM 21's `__introsort`, both partitions, the guarded, unguarded and incomplete insertion sorts, and `std::partial_sort`'s heap sort with Floyd's pop). Its unit tests pin checksums of the orders Apple clang 21's libc++ leaves over 1,540 inputs, along with macOS mongod's two recorded orders.
- `sorter::std_sort_order` uses it where `stl::LIBCXX` is set (the macOS arm64 build).

Fixes RM-526 (tests/backlog/test_rm_found_2026_11_10_rm395.py::test_tied_sort_spill_storage_size, tests/backlog/test_rm_326_sort_counts.py::test_descending_and_tied_spill_storage_size, tests/backlog/test_rm_found_2026_11_10_rm395.py::test_macos_tied_sort_order)
