# pymongo-validation

Runs **(a curated subset of) pymongo's own test suite, unmodified**,
against an embedded `SecantusDBServer` and emits a markdown report at
`docs/validation-report.md` showing pass / fail / skip per category.
The pass rate is the most honest "how close is SecantusDB to a complete
MongoDB surrogate" number we can publish.

The submodule at `vendor/pymongo-tests/` is checked out at the pinned
upstream tag with zero local edits — `git diff HEAD` inside the
submodule is empty. The integration is entirely external (this
directory's pytest plugin), so the validation runs **the same tests
pymongo runs in its own CI**, just pointed at our embedded server
instead of an orchestration-managed `mongod`.

## How to run

```bash
git submodule update --init --recursive   # pulls vendor/pymongo-tests
uv sync --extra dev                       # adds pytest-json-report
uv run python -m invoke validate          # run + regenerate report
```

The gauge is serial by default, which is how the published number is
measured. For the inner loop, `--jobs N` runs the same tests on N xdist
workers — each with **its own** embedded server and WiredTiger store, and
whole files distributed to workers (`--dist loadfile`) so upstream's
within-file ordering survives:

```bash
uv run python -m invoke validate --jobs 4  # same 1707 tests, ~3x faster
```

Nothing is deselected, so the pass / fail / skip counts must come out the
same as the serial run — if they don't, that is a bug in the parallel
plumbing, not a coverage change. Keep `--jobs` at 4 or below: the
change-stream `awaitData` tests measure real elapsed time and start
flaking under CPU contention.

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
2. In `pytest_load_initial_conftests` — the one hook that runs *before*
   any conftest import, and so *before* pymongo's `helpers_shared.py`
   reads its env vars at import time — the plugin starts an embedded
   `SecantusDBServer(host="127.0.0.1", port=0, storage_path=<fresh
   tempdir>)` (real on-disk WiredTiger via
   `tempfile.mkdtemp(prefix="secantus-pymongo-gauge-")`, never
   `:memory:`) and writes the bound host/port into `DB_IP` and
   `DB_PORT`. `pytest_configure` then re-checks what the helpers
   actually captured, and asks that address for a `serverStatus`
   `secantus` marker, so a plumbing bug can't put a real `mongod`
   behind the gauge. Under `--jobs N` every worker does this for its
   own server.
3. pytest collects from the paths in `pymongo_validation/include_paths.py`.
4. `pytest-json-report` writes a machine-readable result to
   `.validation/raw.json`.
5. `pymongo_validation.generate_report` parses the JSON and emits
   `docs/validation-report.md` grouped by first-level path component
   under `vendor/pymongo-tests/test/`.

Tests gated on topology that SecantusDB doesn't aim to support
(real multi-node replica sets, sharding, transactions w/ rollback,
encryption, auth, TLS, sessions w/ correlation, retryable writes /
reads) self-skip via pymongo's own decorators — those skips are honest
gaps, not failures. Change streams ARE in scope via the single-node
oplog implementation; `hello` advertises a fictional `setName:
"secantus"` so pymongo's topology check accepts `watch()`.

## Files

- `plugin.py` — the pytest plugin that bootstraps the embedded server.
- `include_paths.py` — the curated list of in-scope pytest paths.
- `generate_report.py` — JSON → markdown table renderer.

## Not for distribution

Nothing under `pymongo-validation/` or `vendor/pymongo-tests/` ships in
the SecantusDB wheel or sdist. This is dev-only infrastructure.

## License

This README, like the rest of SecantusDB's written content, is licensed
[CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/). The Python
code in `plugin.py`, `include_paths.py`, and `generate_report.py` is
GPL-2.0-only (the project's code license).
