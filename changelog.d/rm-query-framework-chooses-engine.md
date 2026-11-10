### A query setting's `queryFramework` chooses the engine on the Rust MongoDB server

A query setting can name the engine a query runs in, and the Rust MongoDB server now honours it as mongod 8.2 does. Under `queryFramework: "sbe"` a plain `find` runs in the slot-based engine: its expressions answer and fail as SBE's do (`$mod` by zero is 4848403, not the classic 16610), and its `explain` is the slot-based plan mongod prints, with `explainVersion` "2", the slot program and the SBE stages' own execution counts. Under `"classic"` nothing is pushed into SBE, so a `$group` mongod would otherwise run there stays classic.

The settings also reach what is reported about the query: the plan cache key folds the framework and `reject` in as mongod's does, `serverParameters` names the framework control the setting chose, and slow-query lines and profile entries report the engine, including for each `getMore` of the cursor.

#### Fixed

- `secantus-commands` `query_settings`: `framework()` reads a read's `queryFramework`; `reenter` puts the settings back in place for its slow-query line and profile entry.
- `secantus-commands` explain: `sbe_pushdown` keeps everything classic under `"classic"`; `find_runs_in_sbe` decides a `find`'s engine, which its read, its `getMore`s, its cache key and its `queryFramework` follow; `explain/sbe/find.rs` builds the slot program of a `find` over a forward collection scan with its comparison filter, skip and limit; `serverParameters.internalQueryFrameworkControl` is the setting's where the reply is the query's own explain.
- `secantus-commands` `cache_key`: `planCacheKey` hashes the setting's framework and `reject`.
- `secantus-core` `sbe`: `find_compatible`.
- Fixes RM-418 in part (tests/backlog/test_rm_219_diag_query_settings.py::test_query_framework_chooses_the_engine; also tests/mongod_live/test_sbe_find_explain.py). Still open under RM-418: `showDebugQueryShape`, the 115 refusal and the `config.clusterParameters` reload.
- Filed RM-542 (`aggregate` / `distinct` under `"sbe"`), RM-543 (other `find` shapes under `"sbe"`), RM-544 (`indexHints` in the cache key), RM-545 (`optimizationTimeMillis`).
