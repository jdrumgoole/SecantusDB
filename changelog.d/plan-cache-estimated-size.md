### `$planCacheStats` reports each entry's estimated size on macOS

mongod lists every plan cache entry with `estimatedSizeBytes`, the bytes it reckons the entry takes in its memory. The Rust MongoDB server left the field out. It now computes it the way mongod 8.2.11 does: the entry, its cached index tree (one node per node of the query's match expression, each node the plan tagged with an index carrying a copy of that index's description), the query the entry was created from, and every candidate plan's trial statistics. The figure is a sum over mongod's C++ objects, so it depends on how mongod was built. The server uses the type layout of mongod's macOS arm64 build (libc++), and its figures match mongod's exactly over the shapes measured: one- and two-field indexes, `_id_`, multikey fields, sorts, projections, paging, `$in`, `$ne`, `$nin`, `$not`, `$exists`, `$type`, regexes, `$expr`, long index names, `AND_SORTED` and `SORT_MERGE` candidates, the branches of a rooted `$or`, and the entries a query run in SBE caches.

On Linux the field is still left out until the libstdc++ layout of mongod's x86_64 build has been measured (RM-442). On any platform it is also left out for the entries whose index tree the server does not build yet, such as an `$elemMatch` or an `$or` that is not the root (RM-436). It is never given another build's number.

#### Fixed

- `$planCacheStats` gives each entry mongod's `estimatedSizeBytes` on macOS arm64 (`crates/secantus-commands/src/explain/entry_size.rs`, with the planner's index tags from `plan::index_tags`). Fixes RM-341 (tests/backlog/test_rm_340_342_plan_cache_gaps.py::test_an_entry_reports_its_estimated_size, tests/backlog/test_rm_340_342_plan_cache_gaps.py::test_entries_of_many_shapes_report_mongods_size).

#### Added

- Backlog items found while measuring it, each with a gap test in `tests/backlog/test_rm_found_2026_11_08_rm341.py`: RM-436 (entries still unsized), RM-437 (SORT_MERGE trial counts), RM-438 (`$nin` scan seeks), RM-439 (`$exists` FETCH filter order), RM-440 (`$type` index filter in the trial), RM-441 (a multikey index covering a field that holds no array). RM-442 holds the Linux layout.
