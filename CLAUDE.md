# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

SecantusDB is a **surrogate single-node MongoDB server** written in Python. It speaks the MongoDB wire protocol well enough to satisfy the `pymongo` driver, so application tests can run against it instead of standing up a real `mongod`. The package is `secantus`; the public class is `SecantusDBServer`. "Surrogate" rather than "fake" — it really is a MongoDB server, just intentionally scoped to single-node operation.

The name was chosen to dodge brand-clash risk: an early prototype was called "fongo", a follow-on was called "fongodb", and the current name avoids both the existing "Fongo" brand and any confusion with MongoDB itself. Internal references to `fongo` or `fongodb` are stale — flag and rename to `secantus` (or `SecantusDB` for the brand form).

**In scope:** the subset of the MongoDB wire protocol that `pymongo` actually emits — connection handshake, CRUD, cursors, aggregation, findAndModify, and **change streams** (single-node, oplog-backed; collection / db / cluster scope; resume tokens; `fullDocument: "updateLookup"`; `fullDocumentBeforeChange` pre-images; `awaitData` blocking).

**Explicitly out of scope:** real replica sets, sharding, multi-node consistency. SecantusDB advertises itself as a single-node `secantus` replica-set primary in the `hello` reply (so `pymongo`'s topology machinery accepts change streams), but the topology is fictional — there are no other members, no elections, no cross-node oplog. If a feature only makes sense in a multi-node deployment, SecantusDB does not implement it.

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
- `src/secantus/aggregate.py` — `apply_pipeline(docs, pipeline, ctx)`. Stages: `$match`, `$count`, `$limit`, `$skip`, `$sort`, `$project` (with computed fields), `$addFields`/`$set`, `$unset`, `$unwind`, `$densify` (numeric ranges only — `bounds: "full"` / `[min, max]`, `partitionByFields`, positive `step`; date `unit` deferred), `$replaceRoot`/`$replaceWith`, `$group`, `$lookup` (both simple and `let`/`pipeline` forms; uses an O(N+M) hash-join — `_build_lookup_index` keys foreign docs by their `foreignField` value, with array values expanded element-wise, and `_hash_join_lookup` does dict lookups per outer doc), `$sample`, `$sortByCount`, `$facet`, `$bucket`. `$group` accumulators: `$sum`, `$count`, `$avg`, `$min`, `$max`, `$first`, `$last`, `$push`, `$addToSet`. `PipelineContext` carries the `Storage`, current `db_name`, and a `vars` map (for `$lookup` `let` bindings, threaded through every stage that calls the expression evaluator).
- `src/secantus/cursors.py` — `CursorRegistry`. Per-server, thread-safe map of int64 cursor id → remaining docs. Used by `find` and `aggregate` to support pagination via `getMore`/`killCursors`. Cursors carry a `last_access` timestamp; entries idle longer than `idle_ttl_seconds` (default 600s, matching MongoDB's 10-minute cursor TTL) are pruned opportunistically on every `register` / `next_batch` / `kill` / `len`. The clock is injectable (`time_func`) so tests can drive expiry deterministically.
- `src/secantus/paths.py` — shared dotted-path helpers (`get_path`/`set_path`/`unset_path`/`has_path`/`walk_to_parent`). Used by `update`, `projection`, `aggregate`, and storage's sort.
- `src/secantus/sortkey.py` — pure `encode_value(v)` and `encode_compound([v1, v2, ...])` that produce **byte-sortable** bytes whose lex order matches MongoDB's BSON cross-type sort order. Layout: `<rank_byte><payload>`. Numbers go through a "lexical decimal" form (sign byte + bias-shifted exponent + paired BCD digits + terminator) so int / long / double / Decimal128 collide on equal value and order correctly across the unified numeric type. NaN / ±Infinity get dedicated bracketing markers. Strings, binary, regex are null-escaped (`\x00 → \x00\xff`) so `\x00\x00` is a safe compound separator. `encode_value_directed(v, direction)` bitwise-inverts the bytes when `direction == -1` so the same encoder drives descending indexes.
- `src/secantus/storage.py` — WiredTiger-backed store (same engine MongoDB uses). Four tables in one WT connection:
  - `table:secantus_collections` (key_format=`SS`, value=BSON options blob) — `(db, coll)` registry.
  - `table:secantus_documents` (key_format=`SSu`, value=`u`) — `(db, coll, id_key) → bson.encode(doc)`. `id_key` is `sortkey.encode_value(_id)`: byte-sortable across BSON types, so iterating the table gives MongoDB's natural cross-type sort order (numeric for int/float/Decimal128 with cross-type collision preserved by the lexical-decimal encoding, chronological for `ObjectId`, lexical for strings, etc.). `update_matching(multi=False)` and `find()` without `sort` walk in this natural order, matching `mongod`.
  - `table:secantus_indexes` (key_format=`SSS`, value=`u`) — `(db, coll, name) → bson.encode({key, options})`.
  - `table:secantus_index_entries` (key_format=`SSSu`, value=`u`) — `(db, coll, name, packed) → b""` where `packed = escape(sortkey) + b"\x00\x00" + id_key`. The packed form is in a single trailing `u` column on purpose: WT length-prefixes non-trailing `u` columns, which would break lex order on the sort-key bytes. Maintained on every insert/update/delete.
  WT sessions are thread-affine, kept in `threading.local()`; cursors per session per table are cached and `reset()` between calls. A global `RLock` serializes all public methods so we never have to think about WT's MVCC at the storage layer. `:memory:` is mapped to a `tempfile.mkdtemp()` opened with `in_memory=true` and rmtree'd on `close()`.

### Indexes: equality (incl. compound prefix) + range (incl. compound trailing) + sort

`find_matching` routes through the index entries table for:
- **Single-field filters**: bare equality (`{field: v}`), `$eq`, `$in`, and any combination of `$gt`/`$gte`/`$lt`/`$lte` against a single-field index. When no single-field index covers `field`, a compound index whose leading field is `field` is used instead — equality lookups become prefix scans (`enc(v) + COMPOUND_SEP`), and range bounds are evaluated with a leading-field-only scan that uses `startswith(esc_X + esc_compound_sep)` to identify boundary rows (a literal find/split on the separator is unreliable because an escaped numeric terminator overlaps with the start of the escaped separator).
- **Multi-field bare-equality filters**: when the filter's fields are a leading prefix (set-wise) of an ASC compound index, an exact match (filter covers the whole index) or a prefix scan (strict leading prefix) runs. Filter field order doesn't matter — `{b: 20, a: 1}` finds the same `{a:1, b:1}` index as `{a: 1, b: 20}`. Single-field filters can also use a compound index whose leading field matches.
- **Compound prefix + trailing operator**: filters of the form `{a: 5, b: {$gt: 10}}` (any number of leading bare-equality fields followed by exactly one operator-form field, where that operator field is the next field in the index after the equality prefix) prefix-pin the equalities and apply the operator's bounds to the next column. Supports `$eq`/`$in`/`$gt`/`$gte`/`$lt`/`$lte` on the trailing field.
- **Mixed-direction compound indexes**: compound indexes accept any per-field direction (`{a:1, b:-1}`, `{a:-1, b:-1}`, etc.). Each field is byte-encoded with `encode_value_directed(value, dir)` so the entries table sorts in the index's natural order. Equality prefix lookups, prefix scans, and the trailing-operator path all work; when the trailing field is DESC the operator semantics flip (`$gt` becomes upper-exclusive in byte order), and unique enforcement still uses a prefix probe.

Sort-by-indexed-field rides the same B-tree. The walk direction is chosen so the result is already in sort order: if the index direction matches the sort direction, walk forward; if they're opposite, walk backward. A single-field sort can be served by any index whose leading field matches — single-field or compound, ASC or DESC — with no Python list reversal in the common path. The post-sort step is skipped.

Unique enforcement is a prefix probe on the entries table, not a full scan.

**Multikey fallback**: indexes don't yet support per-element entries for array values, so an index where any doc has a list value on an indexed field is flagged `multikey: True` (sticky — never cleared) at insert / update / `create_index` time. Pickers skip multikey indexes, and `find_matching` falls back to a full scan + `matches()` so array-element queries (e.g. `{tags: "python"}` against `{tags: ["python", "go"]}`) return the correct rows. The flag is persisted in the index options blob and surfaced through `list_indexes`.

`hint` is honored on both `find` and `aggregate`: pass an index name string, a key-spec dict, `"$natural"` (forces a collection scan even when an index would match), or `"_id_"` / `{_id: 1}` (walks doc-table order). An unknown hint surfaces as a `BadValue` (code 2) error to the client. The hint can also align with the sort spec to skip the post-sort step when the leading field matches.

`aggregate` also lifts a leading `$match` stage into the initial fetch's filter so a pipeline starting with `[{$match: {...}}]` benefits from the same index acceleration as `find`. The `$match` stage is then skipped in the pipeline so the filter isn't re-applied.

`explain` reports `IXSCAN` when an index would be used and `COLLSCAN` otherwise. `Storage.explain_plan(...)` mirrors `find_matching`'s routing decisions without executing them and returns `{"kind": "IXSCAN", "index_name", "key_pattern", "direction"}` or `{"kind": "COLLSCAN"}`; the `_explain` command shapes that into MongoDB's `winningPlan` (`FETCH` wrapping an `IXSCAN` inputStage, with `indexName` / `keyPattern` / `direction`). Picker helpers (`_pick_compound_eq_index`, `_pick_compound_range_index`, `_find_leading_field_index`) are shared between the lookup and planning paths.

**Direction support**: single-field and compound indexes accept any per-field direction. The encoder bitwise-inverts the bytes for DESC fields so the WT B-tree gives us the index's natural order with a forward walk. Equality, `$in`, range (`$gt`/`$gte`/`$lt`/`$lte`) — operator semantics flip automatically when targeting a DESC field — and direction-aware sort acceleration all work end-to-end on single-field indexes, and the equality/prefix/trailing-operator paths all work on mixed-direction compound indexes.

**Partial indexes**: indexes accept a `partialFilterExpression` option (e.g. `{status: "active"}`); only docs that `matches()` the expression get entries written, and pickers may use a partial index only when the user query implies the partial filter (every key/value in the partial filter appears with the same bare value in the user filter). Conservative: operator-form clauses or document-level operators in the partial filter aren't recognised as implied. The picker strips partial-filter keys when matching the user filter against the index key spec, so a query like `{status: "active", n: 5}` against a partial index on `{n: 1}` with filter `{status: "active"}` correctly uses the index.

**TTL indexes**: `expireAfterSeconds` is honoured by `Storage.prune_ttl(db, coll, *, now=None)` which walks the collection, deletes docs whose indexed `datetime` field is older than `now - expireAfterSeconds`, and removes their index entries. The clock is injectable so tests can drive expiry deterministically. There is **no background sweeper** — real MongoDB prunes every 60s; SecantusDB requires the caller to invoke `prune_ttl` explicitly. Docs without the TTL field, with non-date values, or with values inside the window are left untouched.

Still missing: multi-field sort acceleration (a sort `{a:1, b:1}` matching a compound index `{a:1, b:1}` would skip post-sort entirely; today only single-field sort is index-accelerated), and `collation`.

Out of scope regardless: text / hashed / wildcard indexes, collation.

**Geo support (Phase 1, no index acceleration)**: `$geoWithin`, `$geoIntersects`, `$near`, `$nearSphere` field operators and `$geoNear` aggregation stage land in `secantus.geo` + `secantus.query` + `secantus.aggregate`. Doc-side accepts GeoJSON (`{type:"Point|Polygon|...", coordinates: ...}`), legacy `[x, y]` pairs, and `{x, y}` / `{lng, lat}` maps. Query-side accepts `$geometry` (GeoJSON), `$box`, `$polygon`, `$center` (planar disk), `$centerSphere` (great-circle cap, radius in radians). Containment and intersection delegate to Shapely (planar — Shapely 2.x); spherical-circle containment uses haversine in `secantus.geo._great_circle_radians` directly. Distance returns meters when spherical (mean-radius `EARTH_RADIUS_METERS = 6_378_100.0` matching `mongod`'s constant) and planar units otherwise. `$geoNear` sorts ascending by distance and attaches the value under `distanceField` — `key` is required (Phase 1 has no index, so we can't infer the geo field). All operators run as full collection scans; index acceleration (2dsphere via S2 cells, 2d via geohash bits) is the Phase 2 work tracked in `tasks/backlog.md`.

### Oplog and change streams

Three more WT tables, in the same connection:

- `table:secantus_oplog` (key_format=`q`, value=BSON) — `seq → entry`. `seq` is a strictly-monotonic int64 minted under the storage `RLock`. Entry shape mirrors mongod's oplog: `ts: Timestamp(secs, ord)`, `op: "i"|"u"|"d"|"c"`, `ns`, `ui` (collection UUID, BSON Binary subtype 4), `o`, `o2`, `wall: datetime`. Updates carry `o = {"$v": 2, "diff": <updateDescription>}` where `diff` is a faithful walk-and-compare from `secantus.diff.compute_update_description` (dotted-path `updatedFields`, `removedFields`, `truncatedArrays`).
- `table:secantus_preimages` (key_format=`q`, value=BSON) — `seq → pre_image_doc`. Only written when the source collection has `changeStreamPreAndPostImages: {enabled: true}` set via `create` / `collMod`. Used to satisfy `fullDocumentBeforeChange` on `update` / `delete` change events.
- `table:secantus_oplog_meta` (key_format=`S`, value=BSON) — single key `"state"` storing `{next_seq, last_ts_secs, last_ts_ord}`. Persisted at the end of every `_emit_oplog`, recovered on startup so `Timestamp` minting and seq numbering are strictly greater than any previously-emitted value.

Retention: `prune_oplog(*, now=None)` drops entries older than `oplog_retention_seconds` (default 1h) and trims to `oplog_max_entries` (default 100k), deleting paired pre-images. Called opportunistically (every 1000 emits) and exposed publicly. No background sweeper — same pattern as `prune_ttl`.

Cross-thread reads (`read_oplog`, `read_preimage`, `oplog_floor_seq`, `find_seq_for_ts`) open a **fresh WT session per call** rather than reusing the per-thread cached session. WiredTiger's MVCC keeps a session's read snapshot until the session commits / resets; reusing the cached session for tailable getMore polls would never observe rows committed by writer threads on other connections. The fresh session is cheap and uniformly correct.

Cluster time: `Storage.current_cluster_time()` returns the next monotonic `Timestamp(secs, ord)` and persists it. Used in `hello`'s `lastWrite.opTime` and the `aggregate` reply's `operationTime`.

`hello` advertises the server as a single-node `secantus` replica-set primary (`setName: "secantus"`, `hosts: [<addr>]`, `primary: <addr>`, `me: <addr>`, `electionId`, `lastWrite.opTime.ts`) so pymongo's `Watch` accepts the topology. Switch off via `SecantusDBServer(..., replica_set_name=None)` for tests that want a pure standalone hello reply.

Tailable cursors live in `CursorRegistry`: change-stream cursor IDs are int64-random (`> 2**32`) to dodge driver assumptions; the entry carries a `producer` closure (reads oplog → projects events), `position_seq`, `await_data`, and an `invalidated` flag. `_get_more` blocks on `Storage._oplog_cv` (a separate `Lock`-backed `Condition`, **not** the storage `RLock`) until a writer notifies via `_emit_oplog` or until the per-call timeout expires. PyMongo doesn't always send `maxTimeMS` on change-stream getMore; the server uses 1s as the default tailable wait so the connection thread can be reaped on shutdown.

Event projection lives in `secantus/changestreams.py`: `project(seq, oplog_entry, *, storage, full_document_mode, full_document_before_change_mode, scope) -> (event, invalidates)`. Resume tokens are `{"_data": "<hex>"}` where the hex is `bson.encode({"s": seq, "t": ts, "n": ns, "k": documentKey._id})` — opaque to pymongo but enough state for resume / `startAtOperationTime` / invalidation. Drops on a watched coll, dropDatabase on a watched db, and rename of a watched coll all surface a final `invalidate` event and end the cursor on the next getMore.

### Type-mapping strategy (the critical decision)

Documents are stored as **opaque BSON blobs**. All filtering, projection, sorting, and updates happen in Python after `bson.decode`. The storage layer never inspects document content. This is deliberate: SecantusDB's whole point is that `pymongo` cannot tell us apart from `mongod`, and any lossy intermediate representation (JSON, native column types, etc.) would break that for ObjectId / Decimal128 / int32-vs-int64 / Date-with-tz / Binary / Regex.

When secondary indexes land they will be WT indexes over typed sort-key columns derived from BSON values — not JSON, not coerced numerics.

## Tooling

- Python 3.12 pinned via `.python-version`. Managed with `uv`. Always invoke Python via `uv run python -m ...` so `pyenv` doesn't intercept.
- Build/admin tasks: `tasks.py` (`invoke`). `invoke test`, `invoke lint`, `invoke fmt`, `invoke docs`, `invoke serve`.
- `pytest` with `pytest-xdist` parallel by default (`addopts = "-n auto"`). Tests must use `port=0` and `:memory:` storage — no shared ports, no shared WT homes.
- **pymongo conformance suite** lives in `pymongo_validation/` + `vendor/pymongo-tests/` (a git submodule pinned to a pymongo release). **pymongo's tests run unmodified** — the submodule has zero local edits (`git diff HEAD` inside it is empty); the integration is entirely external. Run via `uv run python -m invoke validate` — starts an embedded `SecantusDBServer(port=0, storage_path=":memory:")` in a pytest plugin (`pymongo_validation/plugin.py`), sets `DB_IP`+`DB_PORT` (the env vars pymongo's `helpers_shared.py` reads at import time), runs the curated in-scope test set in `pymongo_validation/include_paths.py`, and writes `docs/validation-report.md`. The pass rate is the honest "MongoDB compatibility" gauge — those are pymongo's actual tests, the same ones pymongo's CI runs against `mongod`. To widen coverage when a new in-scope feature lands, add the relevant pymongo test path to `include_paths.py` and re-run validate. Both `vendor/pymongo-tests/` and `pymongo_validation/` are dev-only — `pyproject.toml`'s `sdist.exclude` keeps them out of the published package. Validation also runs weekly on `.github/workflows/validate.yml`.
- **mongo-go-driver conformance suite** lives in `go_validation/` + `vendor/mongo-go-driver/` (git submodule pinned to a mongo-go-driver release). Same pattern as the pymongo suite, with one important shape difference: `go_validation/runner.py` spawns SecantusDB **as a standalone daemon subprocess** (`python -m secantus --port <free> --storage-path :memory:`) and points the go-driver tests at it via `MONGODB_URI`. The go-driver tests then see exactly what they'd see against a real `mongod` over TCP — zero embedding, zero modifications. Run via `uv run python -m invoke validate-go` (requires `go` 1.21+ on PATH). Report at `docs/validation-report-go.md`. The Go driver is type-strict where pymongo is permissive (e.g. cursor.id MUST be int64, not int32) — that's exactly the class of wire-protocol bug the pymongo gauge can't catch. The Go driver also underpins `mongodump`, `mongorestore`, and most non-Python tooling; if it works here, the broader MongoDB ecosystem works here. Dev-only; excluded from sdist/wheel.
- **mongo-node-driver conformance suite** lives in `node_validation/` + `vendor/node-mongodb-native/` (git submodule pinned to a mongo-node-driver release). Same daemon-subprocess shape as the Go suite. Runner does a one-time `npm install` + `npm run build:bundle` then runs `npx mocha --reporter json` with `MONGODB_URI` and `AUTH=noauth` set. Run via `uv run python -m invoke validate-node` (requires Node.js >=20 on PATH). Report at `docs/validation-report-node.md`. Initial include set is restricted to the import-clean subset of unit tests because mongo-node-driver v7.2.0 has 68 unit files using extensionless ESM imports (`from '../../mongodb'`) that need a non-trivial Node loader chain to resolve `.ts` — patching their `.mocharc` would defeat the "unmodified" gauge property, so the include list trades coverage for honesty. To widen, solve the loader problem upstream (or wait for a release that uses extensions). Dev-only; excluded from sdist/wheel.
- **mongo-java-driver conformance suite** lives in `java_validation/` + `vendor/mongo-java-driver/` (git submodule pinned to a mongo-java-driver release). Same daemon-subprocess shape. Runner spawns the daemon, then invokes the driver's bundled `./gradlew --no-daemon -Dorg.mongodb.test.uri=mongodb://...` for the in-scope Gradle modules in `include_modules.py`. The system property is the seam Java's `ClusterFixture` test infrastructure reads. After Gradle exits, the runner copies JUnit XML out of `<module>/build/test-results/test/TEST-*.xml` (so the submodule stays untouched) into `.validation/java-results/`; `generate_report.py` walks them. Run via `uv run python -m invoke validate-java` (requires a JDK >=8 on PATH — `javac`, not just `java`; the runner errors helpfully if only a JRE is present). Report at `docs/validation-report-java.md`. Initial include set is `:bson:test` only (BSON serialization, ~289 unit test files); the integration modules (`:driver-core:test`, `:driver-sync:test`) need a real-mongod topology that's out of scope. Dev-only; excluded from sdist/wheel.
- WiredTiger is **vendored** as a git submodule at `vendor/wiredtiger` (mongodb-7.0.33). The CMake build is driven by `CMakeLists.txt` (scikit-build-core + ExternalProject) and produces self-contained binary wheels via `cibuildwheel` for cp312 + cp313 on macOS arm64, manylinux2014 + musllinux_1_2 x86_64/aarch64, and Windows AMD64. macOS x86_64 is intentionally absent (runner-pool scarcity, Apple Silicon is the active target). `pip install secantus` ships pre-built on supported platforms; users never need `cmake`/`ninja`/`swig`. The `cmake/patch_wt_*.py` scripts apply small idempotent patches to the vendored WT tree at `PATCH_COMMAND` time (off64_t→off_t for musl, Python module SUFFIX/dynamic_lookup, etc.); fix WT-side incompat by extending one of these patchers, not by editing the submodule directly.
- To run a single test serially: `uv run python -m pytest -n0 tests/path::test_name`. The `-p no:xdist` form fails because `addopts` still injects `-n auto`.
- Sphinx docs in `docs/` (Markdown via `myst-parser`, furo theme). Built with `-W` (warnings-as-errors). `invoke docs` to build, `invoke docs-serve` to preview.
- PyPI publishing is OIDC-only via `.github/workflows/publish.yml` on `vX.Y.Z` tags. The workflow refuses to publish if the tag doesn't match `pyproject.toml`'s version. Never run `uv publish` / `twine upload` manually.
- The on-disk repo path is `/Users/jdrumgoole/GIT/SecantusDB`. The package and PyPI name are `secantus`.

## Releases

**The canonical release path: `release-prepare` once in foreground, then `release-finalize` in a foreground retry loop until exit-0.** A release end-to-end takes ~15-25 minutes; the polling phase routinely exceeds the harness's 10-minute per-Bash-call cap. Sub-agents handle this by retrying the idempotent finalize step — a single 10-min Bash call can't cover the whole polling window, but each attempt picks up exactly where the prior one left off because every step short-circuits on already-done state.

The `run_in_background=true` pattern is **broken by design** for agents: dispatching a bg task and then waiting for the completion notification doesn't keep the agent's tool loop alive — agents terminate after the dispatch and the bg process gets killed with them. Foreground + retry is the reliable pattern; the agent's natural tool calls keep the loop active.

Sub-agent invocation pattern (use `general-purpose`, not Explore — it needs to run shell commands):

> **Step 1 — `release-prepare` (single attempt, ~5–7 min).** Bash call with `timeout: 600000`:
>
> ```
> cd /Users/jdrumgoole/GIT/SecantusDB && uv run --no-sync python -m invoke release-prepare X.Y.Z
> ```
>
> Runs pre-flight, full pytest, perf gates, version bump, commit, tag, push, GitHub Release. `READTHEDOCS_TOKEN` auto-loads from `.env` at the repo root. **If this step fails, abort the whole release and report — don't retry.** Failures here mean tests broke or pre-flight rejected the working tree, neither of which retrying fixes.
>
> **Step 2 — `release-finalize` retry loop (up to 4 attempts × 10 min each = 40 min total budget).** For attempt in 1..4:
>
> ```
> cd /Users/jdrumgoole/GIT/SecantusDB && uv run --no-sync python -m invoke release-finalize X.Y.Z
> ```
>
> with `timeout: 600000`. Exit code 0 → done, verify externally and report PASS. Non-zero exit code (typically a SIGKILL at the 10-min wall) → polling was interrupted mid-step; **immediately re-run the same command**. Every step in `release-finalize` is idempotent (publish workflow already concluded → short-circuits; PyPI already lists version → short-circuits; RTD build already finished → short-circuits; etc.), so retries pick up where the prior 10-min window left off. Bail and report FAIL only after **4 consecutive non-zero exits** — that's 40 minutes of polling, well over the 25-minute worst-case release time.
>
> ⚠️ **Watch the Bash response carefully on each finalize attempt.** The harness sometimes auto-backgrounds long-timeout Bash calls — instead of stdout you'll get a `Command running in background with ID: <id>. Output is being written to: <file>` message. **That's a foreground-mode failure**, not the pattern this contract is built on. If you see it: immediately call `TaskStop` with that task ID, then re-issue the same Bash call. The bg task is fine to kill — `release-finalize` is idempotent and the next foreground attempt picks up cleanly.

`invoke release X.Y.Z` is a thin wrapper that calls both phases in sequence — fine for a developer running it locally, **not safe for sub-agents** because the second half exceeds the per-Bash 10-min cap.

Combined pipeline (the two phases together):

1. Pre-flight: branch=`main`, working tree clean (vendored-submodule drift in either ` m vendor/...` or ` M vendor/...` form tolerated, everything else rejects), `HEAD == origin/main`, tag `vX.Y.Z` not already on origin, `READTHEDOCS_TOKEN` available (either in the shell env or in `.env` at the repo root).
2. Full default test suite (parallel, perf-excluded — currently 653 tests).
3. Perf regression gates (serial — six benchmarks with hard upper bounds).
4. Bump `pyproject.toml` + `src/secantus/__init__.py` + `uv.lock`.
5. `git commit -m "Release vX.Y.Z"` + `git tag -a vX.Y.Z` + push both.
6. `gh release create vX.Y.Z --generate-notes` — creates the user-facing GitHub Release page with auto-generated notes (marked `--prerelease` for `aN` / `bN` / `rcN` versions). **End of `release-prepare`.**
7. Wait for the GitHub `Publish to PyPI` workflow to conclude `success`. **Start of `release-finalize`.**
8. Wait for PyPI's JSON API to list the new version under `releases`.
9. Wait for Read the Docs to publish a successful build for the release commit on the `latest` slug.
10. Activate the `vX.Y.Z` RTD slug and wait for its build to finish — gives users a stable deep link to that release's docs.
11. PATCH RTD's `default_version` to `vX.Y.Z` so `secantusdb.readthedocs.io/` redirects to the freshly-released tag instead of the previous default (`stable` was pinned to v0.1.0 because RTD's `stable` only tracks non-prereleases; this step is the cure).

`release-prepare` aborts cleanly on any failure — leaves the working tree as it was before the bump. `release-finalize` is idempotent: every step short-circuits if the desired state is already true (publish workflow already concluded, PyPI already lists the version, RTD build already finished, slug already active, `default_version` already set), so re-running after any timeout or interruption picks up where it left off.

Pre-requisite: an RTD API token with read+write scope, exposed as `READTHEDOCS_TOKEN`. The release tasks resolve it from (1) the process env, then (2) a `READTHEDOCS_TOKEN=…` line in `.env` at the repo root (gitignored — `.env` is on `.gitignore`). Mint at https://app.readthedocs.org/accounts/tokens/. Both `release-prepare` and `release-finalize` refuse to start without it, since steps 10–11 are the whole reason the docs version stays in sync with PyPI.

Do **not** run `git tag` / `git push` / `uv build` / `uv publish` manually for releases, and do **not** edit RTD's default_version through the dashboard — `release-finalize` owns that value. The only sanctioned path is `invoke release-prepare` + `invoke release-finalize` via sub-agent (or `invoke release` for a developer running it directly). The publish workflow rejects tag/version mismatches anyway, and the manual path is easy to get wrong (out-of-sync `__init__.py`, missed `uv.lock`, no RTD/PyPI confirmation, RTD default left dangling).

## Backlog of stubs and stopgaps

`tasks/backlog.md` is the canonical list of commands that are stubbed, features with simplified implementations, and work explicitly deferred from a slice. **Update it whenever you stub something, defer a slice, or discover a limitation. When you fix an item, delete its line.** Future sessions should treat that file as load-bearing — it's the only honest record of where SecantusDB's behaviour diverges from real MongoDB.

## Conventions for changes here

- **Major features and non-trivial updates go in a git worktree on a feature branch — never directly on `main`.** New CRUD operators, aggregation stages, storage-layer changes, wire-protocol additions, indexing work, and similar multi-file changes all qualify. Trivial one-file tweaks (typo fixes, single-line config edits) can stay on `main`. Create a worktree alongside the repo: `git worktree add ../SecantusDB-<branch> -b <branch>`. Develop and run the full test suite there; merge into `main` only when green, then `git worktree remove ../SecantusDB-<branch> && git branch -d <branch>`. This keeps `main` releasable and lets parallel sessions work without colliding.
- New CRUD operators or aggregation stages should land with both a unit test (in `tests/test_query.py` / `tests/test_update.py` / `tests/test_aggregate.py` / `tests/test_expressions.py`) and a `pymongo`-driven integration test in `tests/test_crud.py`. The integration test is the conformance proof; the unit test pins the semantics.
- Layer boundaries to defend: the wire layer never knows about commands; the command layer never knows about SQL; pure operator engines (`query`, `update`, `expressions`, `aggregate`, `projection`) take only docs in and out, no I/O.
- Errors raised inside command handlers are caught by `dispatch` and turned into `{ok: 0, errmsg, code, codeName}`. Don't leak Python tracebacks to the wire.
