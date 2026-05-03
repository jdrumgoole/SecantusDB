# node-validation

Runs **(a curated subset of) mongo-node-driver's own tests, unmodified**,
against an embedded `SecantusDBServer` and emits a markdown report at
`docs/validation-report-node.md`. The pass rate is the analogue of the
pymongo / mongo-go-driver gauges for the official Node.js driver — the
same driver `mongosh` and the JavaScript ecosystem build on.

The submodule at `vendor/node-mongodb-native/` is checked out at the
pinned upstream tag (`v7.2.0`) with zero local edits. The integration
is entirely external.

## How to run

```bash
git submodule update --init --recursive   # pulls vendor/node-mongodb-native
uv run python -m invoke validate-node     # installs node deps + runs + report
```

Requires Node.js >= 20 and npm on `PATH`. On macOS: `brew install node`.
On Linux: distro package or `nvm`.

The first run takes ~1-2 minutes because `npm install` has to fetch
the driver's dev dependencies into `vendor/node-mongodb-native/node_modules/`.
Subsequent runs reuse the install and complete in seconds.

## Initial scope

This baseline runs `test/unit/` only — pure TypeScript unit tests of
driver internals (BSON serialization, command building, error handling,
etc). They're fast (~10s), don't need a real-mongod expectation, and
catch BSON-format regressions reliably.

The driver's `test/integration/` directory has substantially more
coverage but requires a full TypeScript build (`npm run build:bundle`,
~30s), a real-`mongod`-shaped server, and many tests need replica-set /
auth / TLS / CSFLE topology that SecantusDB intentionally doesn't
provide. Adding `test/integration/<subdir>` files to
`include_paths.py` is the way to widen — the runner already handles
the daemon spin-up.

## What's vendored

`vendor/node-mongodb-native/` is a git submodule pinned to a stable
mongo-node-driver release tag. Apache-2.0 licensed; we do not
redistribute — `pyproject.toml`'s `sdist.exclude` keeps it out of the
wheel and sdist. To bump:

```bash
cd vendor/node-mongodb-native && git checkout vX.Y.Z && cd ../..
git add vendor/node-mongodb-native
rm -rf vendor/node-mongodb-native/node_modules    # force fresh npm install
uv run python -m invoke validate-node             # refresh report
git commit -am "Bump node-validation target -> vX.Y.Z"
```

## How the integration works

1. `node_validation/runner.py` checks for `vendor/node-mongodb-native/
   node_modules/mocha`. If missing, runs `npm install --no-audit
   --no-fund --ignore-scripts` (one-time, ~1-2 min).
2. Finds an OS-assigned free port, spawns SecantusDB **as a standalone
   daemon subprocess** (`python -m secantus --host 127.0.0.1 --port
   <free> --storage-path :memory:`). The Node tests see a real
   `mongod`-shaped TCP server, no embedding.
3. Waits for the daemon's TCP listener.
4. Sets `MONGODB_URI=mongodb://<host>:<port>` and `AUTH=noauth` —
   the latter prevents the driver's test bootstrap from falling back
   to the auth-enabled default URI (`mongodb://bob:pwd123@...`).
5. Runs `npx mocha --reporter json <paths>` from inside
   `vendor/node-mongodb-native/`. Output captured to
   `.validation/node-raw.json`.
6. Tears down the daemon (SIGTERM with a SIGKILL fallback).
7. `node_validation.generate_report` parses the mocha JSON and emits
   `docs/validation-report-node.md`.

## Files

- `runner.py` — npm install gate + daemon bootstrap + mocha invocation.
- `include_paths.py` — list of in-scope mocha test paths.
- `generate_report.py` — JSON → markdown table renderer.

## Not for distribution

Nothing under `node_validation/` or `vendor/node-mongodb-native/`
ships in the SecantusDB wheel or sdist. Dev-only.
