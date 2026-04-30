# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

SecantusDB is a fake MongoDB server written in Python. It speaks the MongoDB wire protocol well enough to satisfy the `pymongo` driver, so application tests can run against it instead of a real `mongod`. The package is `secantus`; the public class is `SecantusDBServer`.

The name was chosen to dodge brand-clash risk: an early prototype was called "fongo", a follow-on was called "fongodb", and the current name avoids both the existing "Fongo" brand and any confusion with MongoDB itself. Internal references to `fongo` or `fongodb` are stale — flag and rename to `secantus` (or `SecantusDB` for the brand form).

**In scope:** the subset of the MongoDB wire protocol that `pymongo` actually emits — connection handshake, CRUD, cursors, aggregation, findAndModify.

**Explicitly out of scope:** replica sets, sharding, change streams that require oplog semantics, and anything else that depends on cluster topology. If a feature only makes sense in a multi-node deployment, SecantusDB does not implement it.

The audience is developers who want fast, ephemeral, in-process MongoDB behaviour for tests — not a production-grade emulator.

## Design constraints

- **`pymongo` is the conformance target.** Behaviour is "correct" when a `pymongo` client cannot tell SecantusDB apart from a real `mongod` for the operations it supports. When in doubt, write a test that runs the same code against `pymongo` → SecantusDB and `pymongo` → real MongoDB and assert the responses match.
- **Wire-protocol fidelity over feature completeness.** Prefer returning a faithful "command not supported" error over a half-implemented feature that silently diverges from real server behaviour.
- **Ease of use for the beginning programmer:** starting a server in a test should be one or two lines, with no external processes to manage.

## Architecture

Layers, roughly outermost-in:

- `src/secantus/server.py` — `SecantusDBServer`: TCP accept loop on a daemon thread, one daemon thread per connection. Owns the `Storage` and the `CursorRegistry`. Per-request, builds a fresh `CommandContext(storage, cursors, db_name)` and calls `dispatch`.
- `src/secantus/wire.py` — header (16 bytes, little-endian), `OP_MSG` (2013) parse/build, legacy `OP_QUERY` (2004) parse + `OP_REPLY` (1) build for the initial `pymongo` handshake. `OP_MSG` kind-1 document sequences are merged into the body before dispatch (server-side).
- `src/secantus/commands.py` — single dispatch table keyed on the first key of the request doc. Handshake (`hello`/`isMaster`/`ping`/`buildInfo`/etc.) and CRUD (`insert`/`find`/`update`/`delete`/`count`/`drop`/`aggregate`/`findAndModify`/`listCollections`/...). Errors raised by handlers are caught and turned into `{ok: 0, errmsg, code, codeName}`. Unknown commands return `code: 59 CommandNotFound` so the connection survives.
- `src/secantus/query.py` — pure `matches(doc, filter, vars=None)`. Field-level operators: `$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`/`$in`/`$nin`/`$exists`/`$not`/`$regex`+`$options`/`$type`/`$size`/`$all`/`$mod`/`$elemMatch`. Document-level operators: `$and`/`$or`/`$nor`/`$expr` (delegates to `expressions.py`, vars threaded through). Dotted paths walk into both maps and arrays.
- `src/secantus/projection.py` — `apply_projection(doc, spec)` for `find()`'s `projection` argument. Inclusion/exclusion modes, `_id` defaults, dotted paths, plus the `$elemMatch` projection operator that returns the first array element matching a sub-filter.
- `src/secantus/update.py` — pure `apply_update(doc, update)`. Operators: `$set`/`$unset`/`$inc`/`$mul`/`$min`/`$max`/`$push`/`$pull`/`$addToSet`/`$pop`/`$rename`. Replacement-style updates preserve `_id`. Mixing operators with replacement fields is rejected.
- `src/secantus/expressions.py` — pure `evaluate(expr, doc, vars=None)`. The aggregation expression language: field paths (`"$x.y"`), `$$varname` user vars + `$$ROOT`/`$$CURRENT`, `$literal`, arithmetic, comparison, logical, `$cond`/`$ifNull`, `$size`, dates (`$year`/`$month`/`$dayOfMonth`/`$dayOfWeek`/`$hour`/`$minute`/`$second`/`$dateToString`), strings (`$concat`/`$split`/`$trim`/`$ltrim`/`$rtrim`/`$substrCP`/`$strLenCP`/`$indexOfCP`/`$toLower`/`$toUpper`/`$toString`), arrays (`$arrayElemAt`/`$first`/`$last`/`$slice`/`$concatArrays`/`$reverseArray`/`$in`/`$filter`/`$map`/`$reduce`), conversions (`$toInt`/`$toDouble`/`$toBool`/`$toDecimal`). Used by the aggregation pipeline and by `$expr` in queries.
- `src/secantus/aggregate.py` — `apply_pipeline(docs, pipeline, ctx)`. Stages: `$match`, `$count`, `$limit`, `$skip`, `$sort`, `$project` (with computed fields), `$addFields`/`$set`, `$unset`, `$unwind`, `$replaceRoot`/`$replaceWith`, `$group`, `$lookup` (both simple and `let`/`pipeline` forms), `$sample`, `$sortByCount`, `$facet`, `$bucket`. `$group` accumulators: `$sum`, `$count`, `$avg`, `$min`, `$max`, `$first`, `$last`, `$push`, `$addToSet`. `PipelineContext` carries the `Storage`, current `db_name`, and a `vars` map (for `$lookup` `let` bindings, threaded through every stage that calls the expression evaluator).
- `src/secantus/cursors.py` — `CursorRegistry`. Per-server, thread-safe map of int64 cursor id → remaining docs. Used by `find` and `aggregate` to support pagination via `getMore`/`killCursors`.
- `src/secantus/paths.py` — shared dotted-path helpers (`get_path`/`set_path`/`unset_path`/`has_path`/`walk_to_parent`). Used by `update`, `projection`, `aggregate`, and storage's sort.
- `src/secantus/sortkey.py` — pure `encode_value(v)` and `encode_compound([v1, v2, ...])` that produce **byte-sortable** bytes whose lex order matches MongoDB's BSON cross-type sort order. Layout: `<rank_byte><payload>`. Numbers go through a "lexical decimal" form (sign byte + bias-shifted exponent + paired BCD digits + terminator) so int / long / double / Decimal128 collide on equal value and order correctly across the unified numeric type. NaN / ±Infinity get dedicated bracketing markers. Strings, binary, regex are null-escaped (`\x00 → \x00\xff`) so `\x00\x00` is a safe compound separator.
- `src/secantus/storage.py` — WiredTiger-backed store (same engine MongoDB uses). Four tables in one WT connection:
  - `table:secantus_collections` (key_format=`SS`, value=BSON options blob) — `(db, coll)` registry.
  - `table:secantus_documents` (key_format=`SSu`, value=`u`) — `(db, coll, id_key) → bson.encode(doc)`. `id_key` is the canonical-numeric-or-BSON-blob form so int/float/Decimal128 collide on `_id`.
  - `table:secantus_indexes` (key_format=`SSS`, value=`u`) — `(db, coll, name) → bson.encode({key, options})`.
  - `table:secantus_index_entries` (key_format=`SSSu`, value=`u`) — `(db, coll, name, packed) → b""` where `packed = escape(sortkey) + b"\x00\x00" + id_key`. The packed form is in a single trailing `u` column on purpose: WT length-prefixes non-trailing `u` columns, which would break lex order on the sort-key bytes. Maintained on every insert/update/delete.
  WT sessions are thread-affine, kept in `threading.local()`; cursors per session per table are cached and `reset()` between calls. A global `RLock` serializes all public methods so we never have to think about WT's MVCC at the storage layer. `:memory:` is mapped to a `tempfile.mkdtemp()` opened with `in_memory=true` and rmtree'd on `close()`.

### Indexes: equality (incl. compound prefix) + range (incl. compound trailing) + sort

`find_matching` routes through the index entries table for:
- **Single-field filters**: bare equality (`{field: v}`), `$eq`, `$in`, and any combination of `$gt`/`$gte`/`$lt`/`$lte` against a single-field ASC index.
- **Multi-field bare-equality filters**: when the filter's fields are a leading prefix (set-wise) of an ASC compound index, an exact match (filter covers the whole index) or a prefix scan (strict leading prefix) runs. Filter field order doesn't matter — `{b: 20, a: 1}` finds the same `{a:1, b:1}` index as `{a: 1, b: 20}`. Single-field filters can also use a compound index whose leading field matches.
- **Compound prefix + trailing operator**: filters of the form `{a: 5, b: {$gt: 10}}` (any number of leading bare-equality fields followed by exactly one operator-form field, where that operator field is the next field in the index after the equality prefix) prefix-pin the equalities and apply the operator's bounds to the next column. Supports `$eq`/`$in`/`$gt`/`$gte`/`$lt`/`$lte` on the trailing field.

Sort-by-indexed-field rides the same B-tree: if `sort` is a single-field `+1` or `-1` on a field with an ASC index, results come back in order (reversed for `-1`) and the post-sort step is skipped.

Unique enforcement is a prefix probe on the entries table, not a full scan.

Still missing: native descending-direction indexes (we only build `{field: 1}` indexes; sort `-1` reverses in Python), `hint` actually picking an index (accepted and ignored), and operator forms on a compound's leading field with no equality prefix (single-field-only-index path still applies — but a compound's leading field accepts only bare equality today).

Out of scope regardless: text / geo / hashed / wildcard indexes, `partialFilterExpression`, TTL semantics (`expireAfterSeconds` is accepted but no expiration), collation.

### Type-mapping strategy (the critical decision)

Documents are stored as **opaque BSON blobs**. All filtering, projection, sorting, and updates happen in Python after `bson.decode`. The storage layer never inspects document content. This is deliberate: SecantusDB's whole point is that `pymongo` cannot tell us apart from `mongod`, and any lossy intermediate representation (JSON, native column types, etc.) would break that for ObjectId / Decimal128 / int32-vs-int64 / Date-with-tz / Binary / Regex.

When secondary indexes land they will be WT indexes over typed sort-key columns derived from BSON values — not JSON, not coerced numerics.

## Tooling

- Python 3.12 pinned via `.python-version`. Managed with `uv`. Always invoke Python via `uv run python -m ...` so `pyenv` doesn't intercept.
- Build/admin tasks: `tasks.py` (`invoke`). `invoke test`, `invoke lint`, `invoke fmt`, `invoke docs`, `invoke serve`.
- `pytest` with `pytest-xdist` parallel by default (`addopts = "-n auto"`). Tests must use `port=0` and `:memory:` storage — no shared ports, no shared WT homes.
- WiredTiger ships as `wiredtiger>=11.3.1` from PyPI. There are no binary wheels yet, so installing from source needs `cmake`, `ninja`, and `swig` available in `PATH`. On macOS: `uv tool install cmake && uv tool install ninja && brew install swig`. Building binary wheels via `cibuildwheel` is the next infra task.
- To run a single test serially: `uv run python -m pytest -n0 tests/path::test_name`. The `-p no:xdist` form fails because `addopts` still injects `-n auto`.
- Sphinx docs in `docs/` (Markdown via `myst-parser`, furo theme). Built with `-W` (warnings-as-errors). `invoke docs` to build, `invoke docs-serve` to preview.
- PyPI publishing is OIDC-only via `.github/workflows/publish.yml` on `vX.Y.Z` tags. The workflow refuses to publish if the tag doesn't match `pyproject.toml`'s version. Never run `uv publish` / `twine upload` manually.
- The on-disk repo path is still `/Users/jdrumgoole/GIT/fongo` (legacy from the `fongo` → `fongodb` → `secantus` rename history); the package and PyPI name are `secantus`. Renaming the directory is fine, not required.

## Backlog of stubs and stopgaps

`tasks/backlog.md` is the canonical list of commands that are stubbed, features with simplified implementations, and work explicitly deferred from a slice. **Update it whenever you stub something, defer a slice, or discover a limitation. When you fix an item, delete its line.** Future sessions should treat that file as load-bearing — it's the only honest record of where SecantusDB's behaviour diverges from real MongoDB.

## Conventions for changes here

- New CRUD operators or aggregation stages should land with both a unit test (in `tests/test_query.py` / `tests/test_update.py` / `tests/test_aggregate.py` / `tests/test_expressions.py`) and a `pymongo`-driven integration test in `tests/test_crud.py`. The integration test is the conformance proof; the unit test pins the semantics.
- Layer boundaries to defend: the wire layer never knows about commands; the command layer never knows about SQL; pure operator engines (`query`, `update`, `expressions`, `aggregate`, `projection`) take only docs in and out, no I/O.
- Errors raised inside command handlers are caught by `dispatch` and turned into `{ok: 0, errmsg, code, codeName}`. Don't leak Python tracebacks to the wire.
