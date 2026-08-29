# mongo-node-driver Validation Report

Generated 2026-08-24 — SecantusDB 0.6.0b15 vs mongo-node-driver 7e53685952f2 (`vendor/node-mongodb-native/`).

Run `uv run python -m invoke validate-node` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver gauges for the official Node.js driver — the same driver `mongosh` and the JavaScript ecosystem build on.

## Summary by category

| Category | Passed | Failed | Pending | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `integration` | 358 | 1 | 5 | 364 | 99.7% |
| **Overall** | **358** | **1** | **5** | **364** | **99.7%** |

## Failures (1)

First 30 failed test titles for triage:

```
integration :: Find should correctly sort using text search in find
```

## How this is generated

**mongo-node-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/node-mongodb-native/` is checked out at the pinned upstream tag with zero local edits. `node_validation/runner.py` ensures `node_modules/` is installed (one-time `npm install` + `npm run build:bundle`), then does a two-phase spawn: phase 1 boots `python -m secantus --port 27018 --storage-path <tempdir> --standalone` without `--auth` and uses pymongo to createUser `root-user` (root role); phase 2 stops that daemon and restarts on the same tempdir **with `--auth`**, so the user record persists and the server now enforces auth. `MONGODB_URI=mongodb://root-user:password@127.0.0.1:27018/?authSource=admin` and `AUTH=auth` make the driver's `test/tools/runner/hooks/configuration.ts` honour our URI rather than fall back to the `bob:pwd123` default. Then `npx mocha --config test/mocha_mongodb.js --reporter json <paths>`.

These are **integration tests** under `test/integration/` — every test opens a real `MongoClient` against the SecantusDB daemon and exchanges wire commands end-to-end. The pass rate is therefore a true measure of SecantusDB's compatibility with the Node.js driver, not of the driver's own pure-code logic.

The include set is currently narrow on purpose: a single broken test in a change-streams or sessions file can pin the runner indefinitely on a tailable getMore that never completes. Each new file is added to `include_paths.py` only after a manual confirmation that it terminates within the runner's wall-clock guard (600 s by default).
