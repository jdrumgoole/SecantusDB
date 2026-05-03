# go-validation

Runs **(a curated subset of) mongo-go-driver's own tests, unmodified**,
against an embedded `SecantusDBServer` and emits a markdown report at
`docs/validation-report-go.md`. The pass rate is the analogue of the
pymongo conformance gauge for the official Go driver — same shape,
different wire-protocol pickiness.

This complements `pymongo_validation/`. pymongo accepts BSON int32 vs
int64 silently; the Go driver enforces the spec. So bugs that pymongo
shrugs at (e.g. `cursor.id` encoded as int32) fail loudly here. The
Go driver is also what `mongodump` / `mongorestore` and most non-Python
tooling are built on, so the Go gauge is the closer signal for "does
SecantusDB work with the broader MongoDB ecosystem?"

The submodule at `vendor/mongo-go-driver/` is checked out at the pinned
upstream tag (`v2.6.0`) with zero local edits — `git diff HEAD` inside
the submodule is empty. The integration is entirely external.

## How to run

```bash
git submodule update --init --recursive   # pulls vendor/mongo-go-driver
                                          # AND its nested testdata/specifications
uv run python -m invoke validate-go       # builds + runs + regenerates report
```

The `--recursive` flag is critical — `mongo-go-driver` itself has a
nested `testdata/specifications/` submodule that holds the MongoDB
driver-spec JSON corpus. Without it the bson-corpus tests all fail
on missing files (silently, in the form of FAIL with no useful
error). The `invoke validate-go` task checks for it and re-runs
submodule init if it's missing.

Requires the Go toolchain on `PATH` (1.21+). On macOS:
`brew install go`. On Linux: distro package or upstream tarball.

## What's vendored

`vendor/mongo-go-driver/` is a git submodule pinned to a stable
mongo-go-driver release tag. Apache-2.0 licensed; we do not
redistribute — `pyproject.toml`'s `sdist.exclude` keeps it out of the
wheel and sdist. To bump:

```bash
cd vendor/mongo-go-driver && git checkout vX.Y.Z && cd ../..
git add vendor/mongo-go-driver
uv run python -m invoke validate-go       # refresh the report
git commit -am "Bump go-validation target -> vX.Y.Z"
```

## How the integration works

1. `go_validation/runner.py` finds an OS-assigned free port, then
   spawns SecantusDB **as a standalone daemon subprocess** — the same
   thing you'd run by hand: `python -m secantus --host 127.0.0.1
   --port <free> --storage-path :memory:`. The go-driver tests see a
   real `mongod`-shaped TCP server, no embedding.
2. Waits for the daemon's TCP listener to accept a connection.
3. Sets `MONGODB_URI` to `mongodb://<host>:<port>` — the seam
   mongo-go-driver's `internal/integtest.MongoDBURI` and
   `internal/integration/mtest` read at test setup.
4. Runs `go test -json -count=1 <packages>` from inside
   `vendor/mongo-go-driver/`. Output is captured to
   `.validation/go-raw.ndjson`.
5. Tears down the daemon (SIGTERM with a SIGKILL fallback).
6. `go_validation.generate_report` parses the NDJSON and emits
   `docs/validation-report-go.md` grouped by Go package.

Tests gated on topology (`mtest.RequiresReplicaSet`,
`mtest.RequiresSharded`, etc.) self-skip when the server doesn't match
— those skips are honest gaps, not failures.

## Files

- `runner.py` — embedded server bootstrap + `go test` invocation.
- `include_packages.py` — list of in-scope Go packages.
- `generate_report.py` — NDJSON → markdown table renderer.

## Not for distribution

Nothing under `go_validation/` or `vendor/mongo-go-driver/` ships in
the SecantusDB wheel or sdist. Dev-only.
