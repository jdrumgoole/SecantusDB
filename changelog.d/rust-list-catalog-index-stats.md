### `$listCatalog`, `$indexStats` use counts and `notablescan` on the Rust MongoDB server

#### Added

- `$listCatalog`, collectionless on `admin` or on one collection, lists
  every collection, view and time-series collection in mongod 8.2.11's
  shape, with mongod's refusals (6200600, 73, 40602, 40600). `ident` and
  `idxIdent` name this store's own WiredTiger tables, and `multikeyPaths`
  is left out (Joe's decision, 2026-10-15).
- `setParameter notablescan`: a query mongod would answer with a
  collection scan is refused with 291 across `find`, `count`, `distinct`,
  `aggregate` (and its `$lookup` / `$unionWith` / `$graphLookup` reads),
  `update`, `delete`, `findAndModify` and `explain`; an empty filter, a
  hinted index and an index-served sort still run, as on mongod.

#### Fixed

- `$indexStats`' `accesses.ops` counts each plan that reads an index, as
  mongod does (not `explain`, a `getMore` or a read of everything), from
  the index's creation; a restart starts the counts again. The planner's
  catalog is now cached per collection, so the counting costs about
  1.5 µs a `find`.
- Fixes RM-035's `notablescan` (`tests/backlog/test_rm_029_056.py::test_notablescan_parameter`)
  and parts of RM-040 (`::test_list_catalog_stage`, `::test_index_stats_counts_use`).
  Pinned besides by `crates/secantus-storage-adapter/tests/catalog_stats_wt.rs`
  and `notablescan_wt.rs`.
