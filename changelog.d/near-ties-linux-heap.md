### Equal-distance `$near` results, merged sorts and `$stdDev` digits follow mongod's Linux build

Where mongod's builds disagree, the Linux build is the one SecantusDB imitates. Several answers on the Rust MongoDB server still came from the macOS build, which uses a different C++ library and fuses floating-point multiply-adds. `$near` returned documents at the same distance in the macOS build's order, and so did a sort merged from several index scans. `$group`'s `$stdDevPop` and `$stdDevSamp` were sometimes off in the last digit, and so were `2d` `$geoNear` distances. All of these now match mongod 8.2.12 on Linux, checked against x86-64 and arm64 Linux mongod.

#### Fixed

- `secantus-core` (`heap`): the priority queue behind `$near`'s result buffer, SORT_MERGE's heads and the S2 / 2d region coverers is now libstdc++'s `push_heap` / `pop_heap` (shared with `stl`), not libc++'s. Entries that compare equal come out in the Linux build's order. `stl`'s heap steps now move by swaps, which gives the same order and needs no `Clone`.
- `secantus-core` (`group`): `$stdDevPop` / `$stdDevSamp` round Welford's `m2` step twice, as the Linux build does. They match it on 2000 of 2000 random sets; before, 60 of 300 were off.
- `secantus-core` (`geoshape`, `geoorder::near`, `geohash2d`): a `2d` `$geoNear` distance is mongod's `distance()`, with each step rounded, where it used to be `hypot`. It matches on 2000 of 2000 random points; before, 18 of 300 were off.
- Fixes RM-231 (tests/backlog/test_rm_029_056.py::test_near_ties_in_scan_order)
- Regression tests: `tests/backlog/test_rm_029_056.py::test_std_dev_group_rounds_as_the_linux_build`, `tests/backlog/test_rm_029_056.py::test_2d_geo_near_distance_as_the_linux_build`, `heap::tests::equal_entries_leave_in_heap_order`, `geoorder::tests::near_ties_come_in_the_queues_order`; the SORT_MERGE and `$near` pins in `secantus-storage-adapter`'s `find_order_wt` / `geo_order_wt` now hold the Linux answers.
- Filed RM-393: the S2 geometry (`s2legacy`) still computes with the macOS build's fused arithmetic. On Linux, a LineString's distance from its own vertex is 0; here it is 6.4e-12 m (`tests/backlog/test_rm_found_2026_11_07e.py::test_a_line_through_the_centre_is_at_distance_zero`).
