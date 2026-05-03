# mongo-node-driver Validation Report

Generated 2026-05-03 — SecantusDB 0.2.0a8 vs mongo-node-driver 7e53685952f2 (`vendor/node-mongodb-native/`).

Run `uv run python -m invoke validate-node` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver gauges for the official Node.js driver — the same driver `mongosh` and the JavaScript ecosystem build on.

## Summary by category

| Category | Passed | Failed | Pending | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `unit` | 460 | 0 | 10 | 470 | 100.0% |
| **Overall** | **460** | **0** | **10** | **470** | **100.0%** |

## How this is generated

**mongo-node-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/node-mongodb-native/` is checked out at the pinned upstream tag with zero local edits. `node_validation/runner.py` ensures `node_modules/` is installed (one-time `npm install`), spawns `python -m secantus --host 127.0.0.1 --port <free> --storage-path :memory:` as a subprocess, exports `MONGODB_URI` (the env var the driver's `test/tools/runner/hooks/configuration.ts` reads at bootstrap) and `AUTH=noauth` (so the bootstrap doesn't fall back to the auth-enabled default URI), then runs `npx mocha --reporter json <paths>` for the in-scope set in `node_validation/include_paths.py`.

Tests gated on real-mongod expectations (replica-set primary elections, topology change events, mongocryptd, KMS, etc.) are out of scope and not included in the in-scope path list. The pass rate above measures how well SecantusDB satisfies the parts of the Node.js driver's contract we DO aim to support.
