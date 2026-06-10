# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

SecantusDB is a **surrogate single-node MongoDB server** written in Python. It speaks the MongoDB wire protocol well enough to satisfy the `pymongo` driver, so application tests can run against it instead of standing up a real `mongod`. The package is `secantus`; the public class is `SecantusDBServer`. "Surrogate" rather than "fake" — it really is a MongoDB server, just intentionally scoped to single-node operation.

The name was chosen to dodge brand-clash risk: an early prototype was called "fongo", a follow-on was called "fongodb", and the current name avoids both the existing "Fongo" brand and any confusion with MongoDB itself. Internal references to `fongo` or `fongodb` are stale — flag and rename to `secantus` (or `SecantusDB` for the brand form).

**In scope:** the subset of the MongoDB wire protocol that `pymongo` actually emits — connection handshake, CRUD, cursors, aggregation, findAndModify, and **change streams** (single-node, oplog-backed; collection / db / cluster scope; resume tokens; `fullDocument: "updateLookup"`; `fullDocumentBeforeChange` pre-images; `awaitData` blocking; `splitLargeChangeStreamEvents` envelope — events are never large enough to actually split, so every fragment is `{fragment: 1, of: 1}`, but the field is present when the user opts in).

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
- `src/secantus/aggregate.py` — `apply_pipeline(docs, pipeline, ctx)`. Stages: `$match`, `$count`, `$limit`, `$skip`, `$sort`, `$project` (with computed fields), `$addFields`/`$set`, `$unset`, `$unwind`, `$densify` (`bounds: "full"` / `[min, max]`, `partitionByFields`, positive `step`; date densify with `unit: "week"|"day"|"hour"|"minute"|"second"|"millisecond"` walks by fixed-duration `timedelta`. Variable-length units `month`/`quarter`/`year` rejected — would need `relativedelta`), `$replaceRoot`/`$replaceWith`, `$group`, `$lookup` (both simple and `let`/`pipeline` forms; per-outer-doc lookups go through `Storage.find_matching` whenever the foreign collection has any index whose leading field is `foreignField` — single-field, compound (leading-prefix scan), and multikey (per-element entries) all light up at IXSCAN. No matching index: falls back to an O(N+M) hash-join via `_build_lookup_index` / `_hash_join_lookup`), `$sample`, `$sortByCount`, `$facet`, `$bucket`. `$group` accumulators: `$sum`, `$count`, `$avg`, `$min`, `$max`, `$first`, `$last`, `$push`, `$addToSet`. `PipelineContext` carries the `Storage`, current `db_name`, and a `vars` map (for `$lookup` `let` bindings, threaded through every stage that calls the expression evaluator).
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

**Multikey indexes**: when a doc has an array-valued field that an index covers, `_index_key_variants` writes one entry per array element *plus* a whole-array entry. The whole-array entry handles equality queries against the full array (`{tags: ["py", "go"]}`); per-element entries handle scalar element queries (`{tags: "py"}`), `$in`, and range. Compound indexes with multiple multikey columns take the cartesian product (real `mongod` rejects this; we accept it but the cardinality blow-up is on the user). Storage flags the index `multikey: True` at insert / update / `create_index` time (sticky — never cleared); the flag still drives sort-acceleration decisions (`_compound_index_for_sort` skips multikey because one doc → many index entries breaks natural-order walks), but query planning treats multikey indexes as first-class — equality / range / `$in` lookups all light up at IXSCAN. `_docs_by_id_keys` and `_candidates_iter` dedup id_keys returned from index walks because the same doc can appear via more than one element entry. Uniqueness probes still use the canonical whole-doc `_index_key` (one canonical key per doc, regardless of array shape).

`hint` is honored on both `find` and `aggregate`: pass an index name string, a key-spec dict, `"$natural"` (forces a collection scan even when an index would match), or `"_id_"` / `{_id: 1}` (walks doc-table order). An unknown hint surfaces as a `BadValue` (code 2) error to the client. The hint can also align with the sort spec to skip the post-sort step when the leading field matches.

`aggregate` also lifts a leading `$match` stage into the initial fetch's filter so a pipeline starting with `[{$match: {...}}]` benefits from the same index acceleration as `find`. The `$match` stage is then skipped in the pipeline so the filter isn't re-applied.

`explain` reports `IXSCAN` when an index would be used and `COLLSCAN` otherwise. `Storage.explain_plan(...)` mirrors `find_matching`'s routing decisions without executing them and returns `{"kind": "IXSCAN", "index_name", "key_pattern", "direction"}` or `{"kind": "COLLSCAN"}`; the `_explain` command shapes that into MongoDB's `winningPlan` (`FETCH` wrapping an `IXSCAN` inputStage, with `indexName` / `keyPattern` / `direction`). Picker helpers (`_pick_compound_eq_index`, `_pick_compound_range_index`, `_find_leading_field_index`) are shared between the lookup and planning paths.

**Direction support**: single-field and compound indexes accept any per-field direction. The encoder bitwise-inverts the bytes for DESC fields so the WT B-tree gives us the index's natural order with a forward walk. Equality, `$in`, range (`$gt`/`$gte`/`$lt`/`$lte`) — operator semantics flip automatically when targeting a DESC field — and direction-aware sort acceleration all work end-to-end on single-field indexes, and the equality/prefix/trailing-operator paths all work on mixed-direction compound indexes.

**Partial indexes**: indexes accept a `partialFilterExpression` option (e.g. `{status: "active"}`); only docs that `matches()` the expression get entries written, and pickers may use a partial index only when the user query implies the partial filter (every key/value in the partial filter appears with the same bare value in the user filter). Conservative: operator-form clauses or document-level operators in the partial filter aren't recognised as implied. The picker strips partial-filter keys when matching the user filter against the index key spec, so a query like `{status: "active", n: 5}` against a partial index on `{n: 1}` with filter `{status: "active"}` correctly uses the index.

**TTL indexes**: `expireAfterSeconds` is honoured by `Storage.prune_ttl(db, coll, *, now=None)` which walks the collection, deletes docs whose indexed `datetime` field is older than `now - expireAfterSeconds`, and removes their index entries. The clock is injectable so tests can drive expiry deterministically. There is **no background sweeper** — real MongoDB prunes every 60s; SecantusDB requires the caller to invoke `prune_ttl` explicitly. Docs without the TTL field, with non-date values, or with values inside the window are left untouched.

Multi-field sort acceleration: a sort spec whose `(field, direction)` tuple list exactly matches — or fully inverts — a compound index's key spec walks the index in forward / backward order and skips the Python post-sort entirely. Picker is strict-shape only (partial-prefix sorts, mixed-direction mismatches, and multikey indexes fall back to COLLSCAN + Python sort). `_compound_index_for_sort` lives in `storage.py` next to `_single_field_index_for`; the planner mirrors the same rules in `explain_plan`.

Still missing: `collation`.

Out of scope regardless: text / hashed / wildcard indexes, collation.

**Geo support**: `$geoWithin`, `$geoIntersects`, `$near`, `$nearSphere` field operators and `$geoNear` aggregation stage live in `secantus.geo` + `secantus.query` + `secantus.aggregate`. Doc-side accepts GeoJSON (`{type:"Point|Polygon|...", coordinates: ...}`), legacy `[x, y]` pairs, and `{x, y}` / `{lng, lat}` maps. Query-side accepts `$geometry` (GeoJSON), `$box`, `$polygon`, `$center` (planar disk), `$centerSphere` (great-circle cap, radius in radians). Containment and intersection delegate to Shapely (planar — Shapely 2.x); spherical-circle containment uses haversine in `secantus.geo._great_circle_radians` directly. Distance returns meters when spherical (mean-radius `EARTH_RADIUS_METERS = 6_378_100.0` matching `mongod`'s constant) and planar units otherwise. `$geoNear` sorts ascending by distance and attaches the value under `distanceField`. **Index acceleration lives in `secantus.geo_index`**: `2dsphere` indexes use S2 cell coverings (s2sphere library) — each indexed geometry writes its covering cells *plus every ancestor back to level 0*, mirroring real-mongo's S2 scheme. Queries compute their own covering+ancestors and do exact point-lookups against the entries table; the candidate verifier filters false positives via Shapely / haversine. `2d` indexes use bit-interleaved geohash buckets at the user's `bits` precision (default 26), with a single `(lo, hi)` bbox range scan per query. Picker (`_pick_geo_index_for_filter` / `_try_geo_index_id_keys`) lives next to the compound-index pickers; `explain` reports `IXSCAN` with the full `keyPattern: {field: "2dsphere"}` shape. Geo indexes are flagged `multikey: True` at creation so the regular pickers skip them for non-geo queries. Cell IDs are encoded with `geo_index.encode_cell` (fixed-width 8-byte big-endian uint64) so the WT B-tree gives lex byte ordering aligned with cell-ID order.

### Oplog and change streams

Three more WT tables, in the same connection:

- `table:secantus_oplog` (key_format=`q`, value=BSON) — `seq → entry`. `seq` is a strictly-monotonic int64 minted under the storage `RLock`. Entry shape mirrors mongod's oplog: `ts: Timestamp(secs, ord)`, `op: "i"|"u"|"d"|"c"`, `ns`, `ui` (collection UUID, BSON Binary subtype 4), `o`, `o2`, `wall: datetime`. Updates carry `o = {"$v": 2, "diff": <updateDescription>}` where `diff` is a faithful walk-and-compare from `secantus.diff.compute_update_description` (dotted-path `updatedFields`, `removedFields`, `truncatedArrays`).
- `table:secantus_preimages` (key_format=`q`, value=BSON) — `seq → pre_image_doc`. Only written when the source collection has `changeStreamPreAndPostImages: {enabled: true}` set via `create` / `collMod`. Used to satisfy `fullDocumentBeforeChange` on `update` / `delete` change events.
- `table:secantus_oplog_meta` (key_format=`S`, value=BSON) — single key `"state"` storing `{next_seq, last_ts_secs, last_ts_ord}`. Persisted at the end of every `_emit_oplog`, recovered on startup so `Timestamp` minting and seq numbering are strictly greater than any previously-emitted value.

Retention: `prune_oplog(*, now=None)` drops entries older than `oplog_retention_seconds` (default 1h) and trims to `oplog_max_entries` (default 100k), deleting paired pre-images. Called opportunistically (every 1000 emits) and exposed publicly. No background sweeper — same pattern as `prune_ttl`.

Periodic noop heartbeats: `Storage.emit_noop_heartbeat()` writes one `{op: "n", ns: "", o: {msg: "periodic noop"}}` row to the oplog. A background thread fires it every `noop_heartbeat_seconds` (default 0 = disabled; mongod's default is 10s and tests can opt in with smaller intervals). Change-stream projection treats `op: "n"` as "skip the event but advance position", so a quiet collection's `postBatchResumeToken` keeps tracking cluster time and stays inside the oplog retention window. Disabled when `enable_oplog=False`.

Cross-thread reads (`read_oplog`, `read_preimage`, `oplog_floor_seq`, `find_seq_for_ts`) open a **fresh WT session per call** rather than reusing the per-thread cached session. WiredTiger's MVCC keeps a session's read snapshot until the session commits / resets; reusing the cached session for tailable getMore polls would never observe rows committed by writer threads on other connections. The fresh session is cheap and uniformly correct.

Cluster time: `Storage.current_cluster_time()` returns the next monotonic `Timestamp(secs, ord)` and persists it. Used in `hello`'s `lastWrite.opTime` and the `aggregate` reply's `operationTime`.

`hello` advertises the server as a single-node `secantus` replica-set primary (`setName: "secantus"`, `hosts: [<addr>]`, `primary: <addr>`, `me: <addr>`, `electionId`, `lastWrite.opTime.ts`) so pymongo's `Watch` accepts the topology. Switch off via `SecantusDBServer(..., replica_set_name=None)` for tests that want a pure standalone hello reply.

Tailable cursors live in `CursorRegistry`: change-stream cursor IDs are int64-random (`> 2**32`) to dodge driver assumptions; the entry carries a `producer` closure (reads oplog → projects events), `position_seq`, `await_data`, and an `invalidated` flag. `_get_more` blocks on `Storage._oplog_cv` (a separate `Lock`-backed `Condition`, **not** the storage `RLock`) until a writer notifies via `_emit_oplog` or until the per-call timeout expires. PyMongo doesn't always send `maxTimeMS` on change-stream getMore; the server uses 1s as the default tailable wait so the connection thread can be reaped on shutdown.

Event projection lives in `secantus/changestreams.py`: `project(seq, oplog_entry, *, storage, full_document_mode, full_document_before_change_mode, scope) -> (event, invalidates)`. Resume tokens are `{"_data": "<hex>"}` where the hex is `bson.encode({"s": seq, "t": ts, "n": ns, "k": documentKey._id})` — opaque to pymongo but enough state for resume / `startAtOperationTime` / invalidation. Drops on a watched coll, dropDatabase on a watched db, and rename of a watched coll all surface a final `invalidate` event and end the cursor on the next getMore.

### Type-mapping strategy (the critical decision)

Documents are stored as **opaque BSON blobs**. All filtering, projection, sorting, and updates happen in Python after `bson.decode`. The storage layer never inspects document content. This is deliberate: SecantusDB's whole point is that `pymongo` cannot tell us apart from `mongod`, and any lossy intermediate representation (JSON, native column types, etc.) would break that for ObjectId / Decimal128 / int32-vs-int64 / Date-with-tz / Binary / Regex.

When secondary indexes land they will be WT indexes over typed sort-key columns derived from BSON values — not JSON, not coerced numerics.

### Engines: pure-Python and Rust both ship → as TWO SEPARATE SERVERS

**Direction (current):** SecantusDB ships **two completely separate servers**,
and a user runs **one or the other** — never a per-operator/per-`Storage`
selection inside one request path:

- **The Python server** — the *original* `SecantusDBServer` (pure-Python `server` /
  `wire` / `commands` / `Storage` / operator engines). No Rust in the request path.
- **The Rust server** — a whole, self-contained Rust server (its own wire /
  dispatch / cursors / accept loop over the pure-Rust `secantus-core` +
  `secantus-storage` + `secantus-wt` crates). No Python in the request path. Its
  Python ergonomic is a **thin embedded lifecycle handle** (`start`/`stop`/
  `address`) — the accept loop runs on a GIL-released Rust thread in-process and
  `pymongo` connects over real TCP; Python is only the launcher, never an operator.

**The authoritative plan is `tasks/rust-server-plan.md`.** It supersedes the
earlier *in-process selectable-engine* model (`SECANTUS_ENGINE` process-wide
selection / the `secantus.engine` per-component shims / the "5e Python `Storage`
adapter + `EngineFallback`"). Read it before doing Rust-side work.

The Rust side is a Cargo workspace under `crates/`: `secantus-core` (pure-Rust
engines + geo + aggregation, **no PyO3**), `secantus-storage` (the full WT-backed
`Storage`, pure-Rust), `secantus-wt` (the WT FFI), plus thin PyO3 binding crates
(`secantus-core-py` → the `_secantus_core` abi3 extension). The PyO3-free split is
exactly what lets the Rust server / standalone `secantusdb` binary reuse the
engines.

- **Current code vs direction.** Today the code still contains the transitional
  in-process selection (`src/secantus/engine.py`: `selected()` / `enabled()` and
  the `query`/`update`/`expressions`/`projection`/`diff`/`sortkey`/`aggregate`
  shims that delegate to `_secantus_core` when enabled; `SECANTUS_ENGINE=python|
  rust|auto`). **This is being retired** in favour of the two-server model: the
  Python server reverts to pure-Python, and `_secantus_core` is kept **only as the
  parity-test vehicle**. Don't build new in-process selection surface; build the
  Rust server (`tasks/rust-server-plan.md` §4, R1–R8).
- **Parity suites stay (the operator oracle).** Each Rust engine is pinned
  byte-for-byte to its pure-Python counterpart by a `tests/test_rust_*_parity.py`
  suite (curated corpus + randomised fuzz); the Rust side returns a "defer to
  Python" signal for constructs it can't reproduce exactly (regex → Python `re`,
  collation, Decimal128 edges, non-ASCII case, etc.). When porting/widening a Rust
  engine, **extend the parity suite first**; never let the two engines drift.
- **Implication for changes:** a change to an operator's semantics must land in
  *both* the Python module and the Rust port (or the Rust port must explicitly
  defer that case), and the parity suite must stay green. Both servers must keep
  the pymongo + driver gauges non-regressing. Plan / phase status / per-engine
  fallback lists: `tasks/rust-server-plan.md` (north star), `tasks/rust-rewrite-
  plan.md`, `tasks/rust-rewrite-phase4-scoping.md`, `tasks/rust-rewrite-spike-
  findings.md`, `tasks/backlog.md` §7. Tooling: `invoke rust-test` / `rust-build`
  / `rust-parity`.

## Tooling

- **Ad-hoc reproducers and cross-driver smokes use `127.0.0.1:27018`** (mongod's standard "alternate" port — out of the way of any real `mongod` running on `27017`, but predictable enough that test-failure messages cite a verbatim address you can hit by hand). Test scripts that need to talk to SecantusDB ad hoc should try `127.0.0.1:27018` first. (Conformance gauges use kernel-assigned ephemeral ports per run — see `/conformance-gauges`.)
- Python 3.12 pinned via `.python-version`. Managed with `uv`. Always invoke Python via `uv run python -m ...` so `pyenv` doesn't intercept.
- Build/admin tasks: `tasks.py` (`invoke`). `invoke test`, `invoke lint`, `invoke fmt`, `invoke docs`, `invoke serve`.
- `pytest` with `pytest-xdist` parallel by default (`addopts = "-n auto"`). Tests must use `port=0` and a unique `storage_path` per test — pytest's `tmp_path` fixture gives both isolation and automatic cleanup. The default suite runs against real on-disk WiredTiger so the schema, persistence, and close-and-reopen paths are continuously exercised (`tests/test_storage.py` includes explicit `Storage` and `SecantusDBServer` reopen-roundtrip tests). The perf-regression suite (`tests/test_perf_regression.py`) is the only test file that stays on `:memory:` storage, because the gates compare against fixed in-memory baselines where on-disk variance would invalidate the thresholds.
- **Driver-conformance gauges** — five gauges run **unmodified** upstream tests against SecantusDB: pymongo (embedded; the headline "MongoDB compatibility" number), and mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver (each a daemon subprocess via `MONGODB_URI`). The other-language gauges catch wire-protocol bugs that pymongo's permissive client misses (e.g. cursor.id-must-be-int64). Invocation (`invoke validate{,-go,-node,-java,-ruby,-all}`), include paths, language toolchain requirements, and current per-gauge caveats live in `/conformance-gauges`. All gauge dirs are dev-only (excluded from sdist/wheel). Validation runs weekly on `.github/workflows/validate.yml`.
- WiredTiger is **vendored** as a git submodule at `vendor/wiredtiger` (mongodb-7.0.33). The CMake build is driven by `CMakeLists.txt` (scikit-build-core + ExternalProject) and produces self-contained binary wheels via `cibuildwheel` for cp312 + cp313 on macOS arm64, manylinux2014 + musllinux_1_2 x86_64/aarch64, and Windows AMD64. macOS x86_64 is intentionally absent (runner-pool scarcity, Apple Silicon is the active target). `pip install secantus` ships pre-built on supported platforms; users never need `cmake`/`ninja`/`swig`. The `cmake/patch_wt_*.py` scripts apply small idempotent patches to the vendored WT tree at `PATCH_COMMAND` time (off64_t→off_t for musl, Python module SUFFIX/dynamic_lookup, etc.); fix WT-side incompat by extending one of these patchers, not by editing the submodule directly.
- To run a single test serially: `uv run python -m pytest -n0 tests/path::test_name`. The `-p no:xdist` form fails because `addopts` still injects `-n auto`.
- Sphinx docs in `docs/` (Markdown via `myst-parser`, furo theme). Built with `-W` (warnings-as-errors). `invoke docs` to build, `invoke docs-serve` to preview.
- PyPI publishing is OIDC-only via `.github/workflows/publish.yml` on `vX.Y.Z` tags. The workflow refuses to publish if the tag doesn't match `pyproject.toml`'s version. Never run `uv publish` / `twine upload` manually.
- The on-disk repo path is `/Users/jdrumgoole/GIT/SecantusDB`. The package and PyPI name are `secantus`.

## Releases and website

Both procedures are managed by skills — they auto-fire on the relevant trigger phrases. To inspect or invoke manually: `/secantusdb-release` and `/secantusdb-website`. The skill files are under `~/.claude/skills/`; treat them as the source of truth and edit them there, not here.

- **`secantusdb-release`** — the two-phase pipeline (`release-prepare` once in foreground, `release-finalize` in a foreground retry loop), the sub-agent contract, foreground-only constraints, the 11-step pipeline (pre-flight → pytest → perf gates → bump → tag → push → GitHub Release → PyPI workflow → PyPI listing → RTD build → RTD slug activation → RTD `default_version`), the `READTHEDOCS_TOKEN` requirement, and the hard prohibitions against manual `git tag`/`uv publish`/RTD-dashboard edits.
- **`secantusdb-website`** — the dedicated `SecantusDB-website` worktree pattern (and why parallel-release auto-stash makes editing `website/` on `main` unsafe), the theme/template merge requirement (`website-dev` is what Pelican builds from, not `main`), the `invoke publish` shortcut (no pytest, no version bump, refuses non-website changes), and the per-release blog-post template (descriptive title + prose body + link bar — never a stub linking out to GitHub).

## Backlog of stubs and stopgaps

`tasks/backlog.md` is the canonical list of commands that are stubbed, features with simplified implementations, and work explicitly deferred from a slice. **Update it whenever you stub something, defer a slice, or discover a limitation. When you fix an item, delete its line.** Future sessions should treat that file as load-bearing — it's the only honest record of where SecantusDB's behaviour diverges from real MongoDB.

## CI is load-bearing — failures are serious bugs, not flakes

After every push to `main` (or to a feature branch with CI configured), check the corresponding GitHub Actions run and **resolve any failures before moving on**. CI is the source of truth — it catches cold-cache races, cross-platform / cross-Python drift, and missing-CI-extra gaps that local-only testing misses. Procedure (`gh run list`/`watch`/`view --log-failed`), the docs-only `paths-ignore` exception (markdown / LICENSE / `docs/**` commits skip CI by design), and the recurring-failure-pattern catalog (xdist install-state races, per-platform sysconf, `pytest-subtests` outcome accounting) all live in `/ci-check`. Add new patterns to that skill as they show up.

## Conventions for changes here

- **Major features and non-trivial updates go in a git worktree on a feature branch — never directly on `main`.** New CRUD operators, aggregation stages, storage-layer changes, wire-protocol additions, indexing work, and similar multi-file changes all qualify. Trivial one-file tweaks (typo fixes, single-line config edits) can stay on `main`. Create a worktree alongside the repo: `git worktree add ../SecantusDB-<branch> -b <branch>`. Develop and run the full test suite there; merge into `main` only when green, then `git worktree remove ../SecantusDB-<branch> && git branch -d <branch>`. This keeps `main` releasable and lets parallel sessions work without colliding.
- New CRUD operators or aggregation stages should land with both a unit test (in `tests/test_query.py` / `tests/test_update.py` / `tests/test_aggregate.py` / `tests/test_expressions.py`) and a `pymongo`-driven integration test in `tests/test_crud.py`. The integration test is the conformance proof; the unit test pins the semantics.
- Layer boundaries to defend: the wire layer never knows about commands; the command layer never knows about SQL; pure operator engines (`query`, `update`, `expressions`, `aggregate`, `projection`) take only docs in and out, no I/O.
- Errors raised inside command handlers are caught by `dispatch` and turned into `{ok: 0, errmsg, code, codeName}`. Don't leak Python tracebacks to the wire.
