Title: Driver gauges widened — 868 integration tests now exercise the wire path
Date: 2026-05-10 21:30:00
Slug: driver-gauges-widened
Author: Joe Drumgoole
Category: Engineering
Tags: drivers, testing

Summary: Three driver gauges (Node, Ruby, Java) widened from curated single-file scopes to broader integration suites. 868 tests now exercise SecantusDB's wire path end-to-end (up from 213), surfacing real divergences from mongod for follow-up.

The driver gauges shipped in v0.5.0b1 were intentionally narrow — one file per driver, just enough to prove the runner plumbing worked end-to-end. This widening round flips that: each gauge now covers as much wire-level CRUD surface as terminates within its wall-clock guard, and the failures it surfaces are real divergences from mongod we can iterate on, not noise from skipped pure-unit tests.

**Node** went from 40 tests in `test/integration/crud/crud_api.test.ts` to **328 / 37** across the full CRUD directory — aggregation, explain, find, find_and_modify, insert, remove, stats, unicode, server_errors, document_validation, abstract_operation. The runner now passes `--timeout 15000` to mocha so a single hung test (tailable getMore polls, change-stream resumes) fails fast at 15s instead of pinning the gauge until the wall-clock budget fires. Server-side, the explain handler now produces the correct aggregate-explain shape — `stages: [{$cursor: {queryPlanner, ...}}, ...]` — and rejects unknown `verbosity` values with `BadValue`. Both were required by the wider gauge.

**Ruby** went from `database_spec.rb` only to five spec files (database, collection_ddl, index/view, address, config) — **265 / 29 / 24 pending**. Files that exercise SDAM retry loops or tailable change-streams (`server_spec`, `cluster_spec`, `auth_spec`, `cursor_spec`) are documented as deferred — they hang the gauge against our single-node-as-RS topology and need their own targeted work before they belong in the gauge.

**Java** went from 60 tests to **172 / 182** by adding `ClientMetadataTest`, `ClusterEventPublishingTest`, and the `UnifiedCrudTest` YAML-driven CRUD spec runner. UnifiedCrudTest brings 111 new passes but also 181 failures — it's the cross-driver CRUD conformance corpus, so each failure is a real wire-shape gap (pipeline-update outcomes, bulk-write result shapes, error-info envelopes) we haven't matched. Honest signal beats a clean number; the failures are now visible and tractable.

**pymongo** and **Go** were left as-is — pymongo's gauge has been a real integration suite from the start, and Go's `./internal/integration/...` already covers everything the upstream test layout exposes. Total test coverage across all five drivers: **868 + 959 pymongo = 1,827** real wire-level integration tests now run on every CI cycle.

[Driver coverage on secantusdb.com](https://secantusdb.com/#drivers)
