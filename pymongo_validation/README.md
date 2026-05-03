# pymongo-validation

Runs (a curated subset of) pymongo's own test suite against an embedded
`SecantusDBServer` and emits a markdown report at
`docs/validation-report.md` showing pass / fail / skip per category.
The pass rate is the most honest "how close is SecantusDB to a complete
MongoDB surrogate" number we can publish.

## How to run

```bash
git submodule update --init --recursive   # pulls vendor/pymongo-tests
uv sync --extra dev                       # adds pytest-json-report
uv run python -m invoke validate          # run + regenerate report
```

## What's vendored

`vendor/pymongo-tests/` is a git submodule pinned to a specific pymongo
release tag (currently `4.17.0`). pymongo is Apache-2.0 licensed; we do
not redistribute it — `pyproject.toml` excludes both
`vendor/pymongo-tests/` and `pymongo_validation/` from the wheel and
sdist. To bump pymongo:

```bash
cd vendor/pymongo-tests && git checkout vX.Y.Z && cd ../..
git add vendor/pymongo-tests
uv run python -m invoke validate          # refresh the report
git commit -am "Bump pymongo validation target -> vX.Y.Z"
```

## How the integration works

1. `pymongo_validation/plugin.py` is loaded as a pytest plugin (`-p`
   flag in the invoke task).
2. In `pytest_configure` — runs *before* test collection, *before*
   pymongo's `helpers_shared.py` reads its env vars at import time —
   the plugin starts an embedded `SecantusDBServer(host="127.0.0.1",
   port=0, storage_path=":memory:")` and writes the bound host/port
   into `DB_IP` and `DB_PORT`.
3. pytest collects from the paths in `pymongo_validation/include_paths.py`.
4. `pytest-json-report` writes a machine-readable result to
   `.validation/raw.json`.
5. `pymongo_validation.generate_report` parses the JSON and emits
   `docs/validation-report.md` grouped by first-level path component
   under `vendor/pymongo-tests/test/`.

Tests gated on topology that SecantusDB doesn't aim to support
(replica set, sharding, change streams, transactions w/ rollback,
encryption, auth, TLS, sessions w/ correlation, retryable writes /
reads) self-skip via pymongo's own decorators — those skips are honest
gaps, not failures.

## Files

- `plugin.py` — the pytest plugin that bootstraps the embedded server.
- `include_paths.py` — the curated list of in-scope pytest paths.
- `generate_report.py` — JSON → markdown table renderer.

## Not for distribution

Nothing under `pymongo-validation/` or `vendor/pymongo-tests/` ships in
the SecantusDB wheel or sdist. This is dev-only infrastructure.
