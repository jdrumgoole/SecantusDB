### `$queryStats` runs on the Rust MongoDB server

`$queryStats` lists what mongod's query stats store has recorded. mongod 8.2.11 records nothing at its defaults (`internalQueryStatsRateLimit: 0`), so on a server left at its defaults the stage answers with an empty list. The Rust MongoDB server used to refuse it as an unknown stage (40324). It now gives mongod's default answer, and it parses the stage's argument the way mongod does, field by field and in mongod's order: `transformIdentifiers` takes `algorithm: "hmac-sha-256"` and a sensitive binData `hmacKey` of 32 bytes or more. The stage is refused anywhere except first in an `{aggregate: 1}` on `admin`, with mongod's codes. That covers a stage nested in a `$facet`, `$lookup` or `$unionWith` sub-pipeline and a view definition. Reading the store needs mongod's privileges: `queryStatsReadTransformed`, plus `queryStatsRead` when the identifiers are not transformed.

`getParameter` reports `internalQueryStatsRateLimit: 0`. This server has no store to record into, so it refuses to change the value (20) and does not accept a setting it could not honour. The store itself remains open as RM-215.

#### Fixed

- `$queryStats`: argument parsing, placement (73 / 40602, and 40600 / 51047 / 31441 in a collectionless sub-pipeline), views, `apiStrict` (323), explain and authorization, as on mongod 8.2.11.
- A binData of subtype 9 is named `vector` in a wrong-binData-type error, as mongod names it (it was `Custom`).
- Partly fixes RM-215 (tests/backlog/test_rm_found_2026_10_20b.py::test_diagnostic_stages, tests/backlog/test_rm_found_2026_10_20b.py::test_query_stats_argument_and_placement). The store stays open under the same ID.

#### Added

- `internalQueryStatsRateLimit` in `getParameter`, with mongod's default (0).
- Backlog: RM-413 (a `$lookup` without `from`) and RM-414 (a collectionless aggregate's explain), both found while working RM-215.
