### 2dsphere geometry rounds as mongod's Linux build does

The Rust MongoDB server's S2 geometry was written against the macOS build of mongod, which fuses floating-point multiply-adds. The Linux build, which SecantusDB imitates, rounds each step. The difference showed where a distance should be exactly zero: measured from one of a LineString's own vertices, a 2dsphere `$geoNear` gave 6.4e-12 metres where Linux mongod gives 0.0. A `$near` therefore put the line after a point or polygon at distance 0 instead of tied with it. The S2 arithmetic now rounds each step, and `$geoNear` measures distance with the same S2 projection `$near` uses. Both now give 0.0, as mongod 8.2.12 on Linux does.

#### Fixed

- `secantus-core` (`s2legacy`): the dot and cross products, latitude, the face ST/UV transforms, the planar orientation test and `S1Interval::expanded` drop `mul_add`. Each is now its C++ original evaluated left to right, with one rounding per operation.
- `secantus-core` (`geoshape`, `geoorder::near`): a spherical `$geoNear` distance is `GeometryContainer::minDistance` via S2's `Project` (`geoorder::near::sphere_distance`, which `$near` also uses). It used to be a separate great-circle approximation that held a line at least 6.4e-12 m away, a floor copied from the macOS build.
- Fixes RM-393 (tests/backlog/test_rm_found_2026_11_07e.py::test_a_line_through_the_centre_is_at_distance_zero)
- Regression test: `geoorder::tests::a_line_from_the_centre_is_at_distance_zero`. The `$near` / `$geoNear` order pins in `secantus-storage-adapter`'s `geo_order_wt` / `geo_queries_wt` now hold Linux's answer, where the line ties at 0 and comes first (`6, 1, 11`, not `1, 11, 6`), and a `$maxDistance: 0` `$near` includes it.
