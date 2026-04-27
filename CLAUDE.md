# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

FongoDB is a fake MongoDB server written in Python. It speaks the MongoDB wire protocol well enough to satisfy the `pymongo` driver, so application tests can run against it instead of a real `mongod`. The package is `fongodb`; the public class is `FongoDBServer`.

The name was chosen to avoid clashing with the existing "Fongo" brand. Older internal references to plain `fongo` are bugs — flag and rename.

**In scope:** the subset of the MongoDB wire protocol that `pymongo` actually emits — connection handshake, CRUD, cursors, aggregation, findAndModify.

**Explicitly out of scope:** replica sets, sharding, change streams that require oplog semantics, and anything else that depends on cluster topology. If a feature only makes sense in a multi-node deployment, FongoDB does not implement it.

The audience is developers who want fast, ephemeral, in-process MongoDB behaviour for tests — not a production-grade emulator.

## Design constraints

- **`pymongo` is the conformance target.** Behaviour is "correct" when a `pymongo` client cannot tell FongoDB apart from a real `mongod` for the operations it supports. When in doubt, write a test that runs the same code against `pymongo` → FongoDB and `pymongo` → real MongoDB and assert the responses match.
- **Wire-protocol fidelity over feature completeness.** Prefer returning a faithful "command not supported" error over a half-implemented feature that silently diverges from real server behaviour.
- **Ease of use for the beginning programmer:** starting a server in a test should be one or two lines, with no external processes to manage.

## Architecture

Layers, roughly outermost-in:

- `src/fongodb/server.py` — `FongoDBServer`: TCP accept loop on a daemon thread, one daemon thread per connection. Owns the `Storage` and the `CursorRegistry`. Per-request, builds a fresh `CommandContext(storage, cursors, db_name)` and calls `dispatch`.
- `src/fongodb/wire.py` — header (16 bytes, little-endian), `OP_MSG` (2013) parse/build, legacy `OP_QUERY` (2004) parse + `OP_REPLY` (1) build for the initial `pymongo` handshake. `OP_MSG` kind-1 document sequences are merged into the body before dispatch (server-side).
- `src/fongodb/commands.py` — single dispatch table keyed on the first key of the request doc. Handshake (`hello`/`isMaster`/`ping`/`buildInfo`/etc.) and CRUD (`insert`/`find`/`update`/`delete`/`count`/`drop`/`aggregate`/`findAndModify`/`listCollections`/...). Errors raised by handlers are caught and turned into `{ok: 0, errmsg, code, codeName}`. Unknown commands return `code: 59 CommandNotFound` so the connection survives.
- `src/fongodb/query.py` — pure `matches(doc, filter)`. Field-level operators: `$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`/`$in`/`$nin`/`$exists`/`$not`/`$regex`+`$options`/`$type`/`$size`/`$all`/`$mod`. Document-level operators: `$and`/`$or`/`$nor`/`$expr` (delegates to `expressions.py`). Dotted paths walk into both maps and arrays.
- `src/fongodb/update.py` — pure `apply_update(doc, update)`. Operators: `$set`/`$unset`/`$inc`/`$mul`/`$min`/`$max`/`$push`/`$pull`/`$addToSet`/`$pop`/`$rename`. Replacement-style updates preserve `_id`. Mixing operators with replacement fields is rejected.
- `src/fongodb/projection.py` — pure `apply_projection(doc, spec)` for `find()`'s `projection` argument. Inclusion/exclusion modes, `_id` defaults, dotted paths.
- `src/fongodb/expressions.py` — pure `evaluate(expr, doc)`. The aggregation expression language: field paths (`"$x.y"`), `$literal`, arithmetic (`$add`/`$subtract`/`$multiply`/`$divide`/`$mod`), comparison (`$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`), logical (`$and`/`$or`/`$not`), `$cond`/`$ifNull`, `$size`, `$concat`, `$toString`/`$toLower`/`$toUpper`. Used both by the aggregation pipeline and by `$expr` in queries.
- `src/fongodb/aggregate.py` — `apply_pipeline(docs, pipeline, ctx)`. Stages: `$match`, `$count`, `$limit`, `$skip`, `$sort`, `$project` (with computed fields), `$addFields`/`$set`, `$unset`, `$unwind`, `$replaceRoot`/`$replaceWith`, `$group`, `$lookup`. `$group` accumulators: `$sum`, `$count`, `$avg`, `$min`, `$max`, `$first`, `$last`, `$push`, `$addToSet`. `$lookup` only supports the simple `from`/`localField`/`foreignField`/`as` form so far. `PipelineContext` carries the `Storage` and current `db_name` for stages that need them.
- `src/fongodb/cursors.py` — `CursorRegistry`. Per-server, thread-safe map of int64 cursor id → remaining docs. Used by `find` and `aggregate` to support pagination via `getMore`/`killCursors`.
- `src/fongodb/paths.py` — shared dotted-path helpers (`get_path`/`set_path`/`unset_path`/`has_path`/`walk_to_parent`). Used by `update`, `projection`, `aggregate`, and storage's sort.
- `src/fongodb/storage.py` — SQLite-backed store. Schema:
  - `_fongodb_collections(db_name, coll_name, options)`
  - `_fongodb_documents(db_name, coll_name, id_key BLOB, doc BLOB)` — full document is `bson.encode(doc)`; `id_key` is the canonical-numeric-or-BSON-blob form so int/float/Decimal128 collide. `RLock`-serialized; `check_same_thread=False` connection. Public `sort_docs` helper used by both find and aggregate's `$sort`.
  - `_fongodb_indexes(db_name, coll_name, index_name, key_spec BLOB, options BLOB)` — see "Indexes are a stopgap" below.

### Indexes are a stopgap

`createIndex` records the definition (so `listIndexes` returns it accurately) and enforces `unique` constraints by full-scanning the collection on every write. **There is no lookup acceleration** — every query still full-scans the document table and filters in Python; `hint` parameters are accepted and ignored. Sparse uniqueness is honored. Compound unique indexes work. `_id_` cannot be dropped.

Out of scope: text / geo / hashed / wildcard indexes, `partialFilterExpression`, TTL semantics (`expireAfterSeconds` is accepted but no expiration), collation.

The eventual fix when this becomes a real bottleneck is **typed sort-key BLOB columns** — 1-byte type tag matching MongoDB's cross-type sort order, then a byte-sortable encoding of the value, with a SQLite B-tree index. Byte-lex comparison on those gives MongoDB-correct ordering and equality through any SQLite index. Don't take a shortcut with raw SQLite-typed columns — SQLite's NULL/INT/REAL/TEXT/BLOB ordering does not match MongoDB's, and Decimal128/ObjectId/Regex have no clean SQLite native type.

### Type-mapping strategy (the critical decision)

Documents are stored as **opaque BSON blobs** in SQLite. All filtering, projection, sorting, and updates happen in Python after `bson.decode`. SQLite is a persistence and indexing layer only; it never inspects document content.

This is deliberate. SQLite's storage classes (NULL/INT/REAL/TEXT/BLOB) cannot represent ObjectId, Decimal128, the int32-vs-int64 distinction, Date with timezone, Binary, or Regex — and FongoDB's whole point is that `pymongo` cannot tell us apart from `mongod`. A lossy JSON-text mapping would break that.

When secondary indexes land they will use **typed sort-key columns** (1-byte type tag matching BSON's cross-type sort order, then a byte-sortable value encoding) — not JSON1, not numeric coercion.

## Tooling

- Python 3.12 pinned via `.python-version`. Managed with `uv`. Always invoke Python via `uv run python -m ...` so `pyenv` doesn't intercept.
- Build/admin tasks: `tasks.py` (`invoke`). `invoke test`, `invoke lint`, `invoke fmt`, `invoke docs`, `invoke serve`.
- `pytest` with `pytest-xdist` parallel by default (`addopts = "-n auto"`). Tests must use `port=0` and `:memory:` storage — no shared ports, no shared SQLite files.
- To run a single test serially: `uv run python -m pytest -n0 tests/path::test_name`. The `-p no:xdist` form fails because `addopts` still injects `-n auto`.
- Sphinx docs in `docs/` (Markdown via `myst-parser`, furo theme). Built with `-W` (warnings-as-errors). `invoke docs` to build, `invoke docs-serve` to preview.
- PyPI publishing is OIDC-only via `.github/workflows/publish.yml` on `vX.Y.Z` tags. The workflow refuses to publish if the tag doesn't match `pyproject.toml`'s version. Never run `uv publish` / `twine upload` manually.
- The on-disk repo path is still `/Users/jdrumgoole/GIT/fongo` (legacy from before the rename); the package and PyPI name are `fongodb`. Renaming the directory is fine, not required.

## Conventions for changes here

- New CRUD operators or aggregation stages should land with both a unit test (in `tests/test_query.py` / `tests/test_update.py` / `tests/test_aggregate.py` / `tests/test_expressions.py`) and a `pymongo`-driven integration test in `tests/test_crud.py`. The integration test is the conformance proof; the unit test pins the semantics.
- Layer boundaries to defend: the wire layer never knows about commands; the command layer never knows about SQL; pure operator engines (`query`, `update`, `expressions`, `aggregate`, `projection`) take only docs in and out, no I/O.
- Errors raised inside command handlers are caught by `dispatch` and turned into `{ok: 0, errmsg, code, codeName}`. Don't leak Python tracebacks to the wire.
