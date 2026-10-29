---
name: python-servers
description: The DEPRECATED Python servers (`SecantusDBServer` / `secantusd-py` and the Python PostgreSQL server `secantus.sql.pgserver`, all under `src/secantus/`) and the storage design they introduced. Fires when touching `src/secantus/`, when a Rust change breaks a Python or parity test, when someone asks how the shared on-disk layout, the index planner (equality/range/compound/multikey/partial/sparse/TTL/geo), the oplog and change streams, or the BSON-blob type mapping work, or when porting a fix between `src/secantus/storage.py` and `crates/secantus-storage`. Holds the layer map, the WiredTiger table layout, the index-planning rules and the change-stream machinery as the Python reference implementation built them.
---

# The Python servers (deprecated) and the design they set

**Deprecated since 2026-10-14** (CLAUDE.md, "Project"): no new work, not the
reference for anything, kept green. Their exemplar was always the real
product. A Rust change that breaks a Python test retires that test with a
written reason; a parity failure moves the parity expectation, never the
Python engine.

This file is still worth reading for Rust work, because the Rust storage
ported most of what it describes: the WiredTiger table layout (the Rust
server's is byte-identical), the index pickers and their sound-not-complete
partial-index implication, the sparse-index gate, multikey handling, the
oplog tables and resume tokens. **When a fix lands in one of these helpers
on either side, grep the other for the same helper** -- the parity suites do
not cover the storage layer, and four index defects (three of them silent
data loss) once outlived their Python fix in `crates/secantus-storage/`.
`tools/probes/index_result_sets.py` compares a server against itself with
and without the index, so it needs no reference server to say which side is
wrong.

Descriptions below record the Python server as built; where a sentence says
what the Rust server does, CLAUDE.md and the Rust backlog are more current.

## Layers

**This section describes the PYTHON reference implementation** (`src/secantus/`).
The Rust servers mirror these layers in `crates/` — see CLAUDE.md's "Engines" for
the crate map. Read this to learn what SecantusDB *does*; read the crates to see what
a user actually runs.

Layers, roughly outermost-in:

- `src/secantus/server.py` — `SecantusDBServer`: TCP accept loop on a daemon thread, one daemon thread per connection. Owns the `Storage` and the `CursorRegistry`. Per-request, builds a fresh `CommandContext(storage, cursors, db_name)` and calls `dispatch`.
- `src/secantus/wire.py` — header (16 bytes, little-endian), `OP_MSG` (2013) parse/build, legacy `OP_QUERY` (2004) parse + `OP_REPLY` (1) build for the initial `pymongo` handshake. `OP_MSG` kind-1 document sequences are merged into the body before dispatch (server-side).
- `src/secantus/commands.py` — single dispatch table keyed on the first key of the request doc. Handshake (`hello`/`isMaster`/`ping`/`buildInfo`/etc.) and CRUD (`insert`/`find`/`update`/`delete`/`count`/`drop`/`aggregate`/`findAndModify`/`listCollections`/...). Errors raised by handlers are caught and turned into `{ok: 0, errmsg, code, codeName}`. Unknown commands return `code: 59 CommandNotFound` so the connection survives.
- `src/secantus/query.py` — pure `matches(doc, filter, vars=None)`. Field-level operators: `$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`/`$in`/`$nin`/`$exists`/`$not`/`$regex`+`$options`/`$type`/`$size`/`$all`/`$mod`/`$elemMatch`. Document-level operators: `$and`/`$or`/`$nor`/`$expr` (delegates to `expressions.py`, vars threaded through). Dotted paths walk into both maps and arrays.
- `src/secantus/projection.py` — `apply_projection(doc, spec)` for `find()`'s `projection` argument. Inclusion/exclusion modes, `_id` defaults, dotted paths, plus the `$elemMatch` projection operator that returns the first array element matching a sub-filter.
- `src/secantus/update.py` — pure `apply_update(doc, update)`. Operators: `$set`/`$unset`/`$inc`/`$mul`/`$min`/`$max`/`$push`/`$pull`/`$addToSet`/`$pop`/`$rename`. Replacement-style updates preserve `_id`. Mixing operators with replacement fields is rejected.
- `src/secantus/expressions.py` — pure `evaluate(expr, doc, vars=None)`. The aggregation expression language: field paths (`"$x.y"`), `$$varname` user vars + `$$ROOT`/`$$CURRENT`, `$literal`, arithmetic, comparison, logical, `$cond`/`$ifNull`, `$size`, dates (`$year`/`$month`/`$dayOfMonth`/`$dayOfWeek`/`$hour`/`$minute`/`$second`/`$dateToString`), strings (`$concat`/`$split`/`$trim`/`$ltrim`/`$rtrim`/`$substrCP`/`$strLenCP`/`$indexOfCP`/`$toLower`/`$toUpper`/`$toString`), arrays (`$arrayElemAt`/`$first`/`$last`/`$slice`/`$concatArrays`/`$reverseArray`/`$in`/`$filter`/`$map`/`$reduce`), conversions — `$convert` plus the `$toInt` / `$toLong` / `$toDouble` / `$toDecimal` / `$toString` / `$toBool` / `$toDate` / `$toObjectId` shorthands, which all **delegate to `$convert`** rather than repeating it (they had drifted, and one path crashed on `Decimal128("Infinity")`). String→number parsing is STRICT — Python's `int()` / `float()` strip whitespace and accept PEP-515 underscores where mongod accepts neither, so `$toInt: " 5 "` used to return a number — and failures carry mongod's per-target reason (`No digits`, `Did not consume whole string.`, `Overflow`, `Leading whitespace`, `Empty string`, `Did not consume any digits`, `Failed to parse string to decimal`), which are not the same set for int / double / decimal. Sweep: 480 shapes at zero divergences against 8.2.11 (2026-09-01). Used by the aggregation pipeline and by `$expr` in queries.
- `src/secantus/aggregate.py` — `apply_pipeline(docs, pipeline, ctx)`. Stages: `$match`, `$count`, `$limit`, `$skip`, `$sort`, `$project` (with computed fields), `$addFields`/`$set`, `$unset`, `$unwind`, `$densify` (`bounds: "full"` / `[min, max]`, `partitionByFields`, positive `step`; date densify with `unit: "week"|"day"|"hour"|"minute"|"second"|"millisecond"` walks by fixed-duration `timedelta`. Variable-length units `month`/`quarter`/`year` rejected — would need `relativedelta`), `$replaceRoot`/`$replaceWith`, `$group`, `$lookup` (both simple and `let`/`pipeline` forms; per-outer-doc lookups go through `Storage.find_matching` whenever the foreign collection has any index whose leading field is `foreignField` — single-field, compound (leading-prefix scan), and multikey (per-element entries) all light up at IXSCAN. No matching index: falls back to an O(N+M) hash-join via `_build_lookup_index` / `_hash_join_lookup`), `$sample`, `$sortByCount`, `$facet`, `$bucket`. `$group` accumulators: `$sum`, `$count`, `$avg`, `$min`, `$max`, `$first`, `$last`, `$push`, `$addToSet`. `PipelineContext` carries the `Storage`, current `db_name`, and a `vars` map (for `$lookup` `let` bindings, threaded through every stage that calls the expression evaluator).
- `src/secantus/cursors.py` — `CursorRegistry`. Per-server, thread-safe map of int64 cursor id → remaining docs. Used by `find` and `aggregate` to support pagination via `getMore`/`killCursors`. Cursors carry a `last_access` timestamp; entries idle longer than `idle_ttl_seconds` (default 600s, matching MongoDB's 10-minute cursor TTL) are pruned opportunistically on every `register` / `next_batch` / `kill` / `len`. The clock is injectable (`time_func`) so tests can drive expiry deterministically.
- `src/secantus/paths.py` — shared dotted-path helpers (`get_path`/`set_path`/`unset_path`/`has_path`/`walk_to_parent`). Used by `update`, `projection`, `aggregate`, and storage's sort.
- `src/secantus/sortkey.py` — pure `encode_value(v)` and `encode_compound([v1, v2, ...])` that produce **byte-sortable** bytes whose lex order matches MongoDB's BSON cross-type sort order. Layout: `<rank_byte><payload>`. Numbers go through a "lexical decimal" form (sign byte + bias-shifted exponent + paired BCD digits + terminator) so int / long / double / Decimal128 collide on equal value and order correctly across the unified numeric type. NaN / ±Infinity get dedicated bracketing markers. Strings, binary, regex are null-escaped (`\x00 → \x00\xff`) so `\x00\x00` is a safe compound separator. `encode_value_directed(v, direction)` bitwise-inverts the bytes when `direction == -1` so the same encoder drives descending indexes.
- `src/secantus/storage.py` — WiredTiger-backed store (same engine MongoDB uses). Four tables in one WT connection:
  - `table:secantus_collections` (key_format=`SS`, value=BSON options blob) — `(db, coll)` registry.
  - `table:secantus_documents_sh<0..15>` (key_format=`SSq`, value=`u`) — `(db, coll, RecordId) → _frame_doc_value(id_key, bson.encode(doc))`, i.e. a `[u32-LE id_key_len][id_key][blob]` frame. The `RecordId` is a global monotonic insertion counter, so a forward walk of the table IS insertion order (mongod's storage order). `id_key` is `sortkey.encode_value(_id)` — byte-sortable across BSON types — and rides *inside* the value because the key no longer carries it (a timeseries doc's suffixed `id_key` isn't derivable from `_id`). Every `_id` lookup / update / delete / uniqueness probe resolves `id_key → RecordId` through the `_id` index below. The table is sharded 16 ways by a hash of `(db, coll)`; `table:secantus_documents` is the legacy pre-shard single table, folded into the shards at open. **Byte-identical to the Rust server's layout** (cross-server backup / PITR portability); a store written before the RecordId change is refused at open (`IncompatibleStorageFormatError`), there is no in-place migration.
  - `table:secantus_natural_seq` (key_format=`SSu`, value=`q`) — `(db, coll, id_key) → RecordId`. This is the **`_id` index**: the only way from an `_id` to its doc row, and where a duplicate `_id` is caught (the doc table keys by the unique RecordId, so it can't reject dups itself). The counter is persisted in the oplog-meta state and recovered on reopen by scanning the doc shards for the max RecordId. Insertion order needs no side index any more — it's the doc table's own key order — so `find()` without `sort`, the `$natural` hint, capped-collection eviction, equal-key sort tie-breaks, the `update_matching` / `delete_matching` candidate scans, and a capped collection's tailable cursor (which anchors on a RecordId via `scan_docs_after_recordid` / `collection_min_recordid`) all walk `_scan_docs` and see mongod's order. `table:secantus_natural` (the old forward `seq → id_key` table) is no longer written; that dropped write is the 4→3 insert write-amp cut.
  - `table:secantus_indexes` (key_format=`SSS`, value=`u`) — `(db, coll, name) → bson.encode({key, options})`.
  - `table:secantus_index_entries` (key_format=`SSSu`, value=`u`) — `(db, coll, name, packed) → b""` where `packed = escape(sortkey) + b"\x00\x00" + RecordId (8 bytes, big-endian)`. The packed form is in a single trailing `u` column on purpose: WT length-prefixes non-trailing `u` columns, which would break lex order on the sort-key bytes. The trailing half is the **RecordId**, so an IXSCAN fetch reads the doc row directly with no `id_key` hop; big-endian keeps entries for one key in RecordId (insertion) order and is fixed-width, so it needs no escaping. Because WT's `key_format` can't distinguish this from the older id_key form, each index records `options.entryFormat = 2` in the catalog and a store with older entries is refused at open (`entryFormat` is stripped from `listIndexes` like `multikey`). Maintained on every insert/update/delete.
  WT sessions are thread-affine, kept in `threading.local()`; cursors per session per table are cached and `reset()` between calls. A global `RLock` serializes all public methods so we never have to think about WT's MVCC at the storage layer. `:memory:` is mapped to a `tempfile.mkdtemp()` opened with `in_memory=true` and rmtree'd on `close()`.

### Indexes: equality (incl. compound prefix) + range (incl. compound trailing) + sort

`find_matching` routes through the index entries table for:
- **Single-field filters**: bare equality (`{field: v}`), `$eq`, `$in`, and any combination of `$gt`/`$gte`/`$lt`/`$lte` against a single-field index. When no single-field index covers `field`, a compound index whose leading field is `field` is used instead — equality lookups become prefix scans (`enc(v) + COMPOUND_SEP`), and range bounds are evaluated with a leading-field-only scan that uses `startswith(esc_X + esc_compound_sep)` to identify boundary rows (a literal find/split on the separator is unreliable because an escaped numeric terminator overlaps with the start of the escaped separator).
- **Multi-field bare-equality filters**: when the filter's fields are a leading prefix (set-wise) of an ASC compound index, an exact match (filter covers the whole index) or a prefix scan (strict leading prefix) runs. Filter field order doesn't matter — `{b: 20, a: 1}` finds the same `{a:1, b:1}` index as `{a: 1, b: 20}`. Single-field filters can also use a compound index whose leading field matches.
- **Compound prefix + trailing operator**: filters of the form `{a: 5, b: {$gt: 10}}` (any number of leading bare-equality fields followed by exactly one operator-form field, where that operator field is the next field in the index after the equality prefix) prefix-pin the equalities and apply the operator's bounds to the next column. Supports `$eq`/`$in`/`$gt`/`$gte`/`$lt`/`$lte` on the trailing field.
- **Mixed-direction compound indexes**: compound indexes accept any per-field direction (`{a:1, b:-1}`, `{a:-1, b:-1}`, etc.). Each field is byte-encoded with `encode_value_directed(value, dir)` so the entries table sorts in the index's natural order. Equality prefix lookups, prefix scans, and the trailing-operator path all work; when the trailing field is DESC the operator semantics flip (`$gt` becomes upper-exclusive in byte order), and unique enforcement still uses a prefix probe.

Sort-by-indexed-field rides the same B-tree. The walk direction is chosen so the result is already in sort order: if the index direction matches the sort direction, walk forward; if they're opposite, walk backward. A single-field sort can be served by any index whose leading field matches — single-field or compound, ASC or DESC — with no Python list reversal in the common path. The post-sort step is skipped.

Unique enforcement is a prefix probe on the entries table, not a full scan.

**Multikey indexes**: when a doc has an array-valued field that an index covers, `_index_key_variants` writes one entry per array element *plus* a whole-array entry. The whole-array entry handles equality queries against the full array (`{tags: ["py", "go"]}`); per-element entries handle scalar element queries (`{tags: "py"}`), `$in`, and range. A dotted path that descends *through* an array — `{"prices.owner_id": 1}` over `{prices: [{owner_id: …}, …]}` — writes one entry per element's leaf value and no whole-array entry (there's no single array to compare against). `paths.get_path_values` is the array-descending path resolver behind all of this (`_index_field_values` adds the whole-array key and the multikey verdict, `_index_field_exists` is the sparse gate); plain `get_path` deliberately does *not* descend, which is what `$set` / projection need. Compound indexes with multiple multikey columns take the cartesian product (real `mongod` rejects this; we accept it but the cardinality blow-up is on the user). Storage flags the index `multikey: True` at insert / update / `create_index` time (sticky — never cleared); the flag still drives sort-acceleration decisions (`_compound_index_for_sort` skips multikey because one doc → many index entries breaks natural-order walks), but query planning treats multikey indexes as first-class — equality / range / `$in` lookups all light up at IXSCAN. `_docs_by_recordids` and `_candidates_iter` dedup the RecordIds returned from index walks because the same doc can appear via more than one element entry. Uniqueness enforcement (`_unique_conflict`, the `createIndexes` pre-check, `find_index_duplicates`) probes **every** key a doc generates, not a canonical one — mongod's unique-multikey rule is "no two docs share any generated key", and a doc indexed through an array has no canonical key among its entries. `_conflict_key_value` recovers the value behind the conflicting key for the dup-key error's `keyValue`.

`hint` is honored on **every** command that takes one — `find`, `count`, `aggregate`, `distinct`, `findAndModify`, and per-statement on `delete` / `update`: pass an index name string, a key-spec dict, `{$natural: 1}` / `{$natural: -1}` (forces a collection scan even when an index would match; `-1` walks it backwards), or `"_id_"` / `{_id: 1}` (walks doc-table order). An unknown hint surfaces as a `BadValue` (code 2) error to the client — on the two write commands as a per-statement `writeErrors` entry with `ok: 1`, which is how mongod reports it. The **string** `"$natural"` is rejected like any other unknown hint: mongod takes only the document form, and `pymongo`'s `.hint("$natural")` produces the string, so accepting it made the natural pymongo call diverge (fixed 2026-08-31, both servers). The hint can also align with the sort spec to skip the post-sort step when the leading field matches.

`maxTimeMS` is **enforced**, not just validated: `secantus/deadline.py` holds a thread-local budget that `dispatch` arms around the handler, and the loops whose length tracks the data poll it (the storage scan and its predicate pass, `count`'s own scan, the write paths' candidate *selection* — before any write, so an expired budget leaves nothing half-applied — the index build, and the aggregation pipeline between stages). An expired budget answers `50 MaxTimeMSExpired`; `createIndexes` wraps it in mongod's index-build envelope. Cooperative, polled every 64 documents. `getMore` is excluded (`_MAX_TIME_MS_NOT_A_DEADLINE`) — there `maxTimeMS` is the awaitData WAIT budget, and arming a deadline would make every tailable poll report a timeout.

`aggregate` also lifts a leading `$match` stage into the initial fetch's filter so a pipeline starting with `[{$match: {...}}]` benefits from the same index acceleration as `find`. The `$match` stage is then skipped in the pipeline so the filter isn't re-applied.

`explain` reports `IXSCAN` when an index would be used and `COLLSCAN` otherwise. `Storage.explain_plan(...)` mirrors `find_matching`'s routing decisions without executing them and returns `{"kind": "IXSCAN", "index_name", "key_pattern", "direction", "multikey", "sorted_by_index"}` or `{"kind": "COLLSCAN"}`; the `_explain` command shapes that into MongoDB's `winningPlan`. For **`find`** that is the full stage tree, not a flat node: `SORT` (with `sortPattern` / `memLimit` / the limit it absorbs) / `SKIP` / `LIMIT` / `PROJECTION_SIMPLE`|`PROJECTION_DEFAULT` wrapping `FETCH(IXSCAN)` or `COLLSCAN`, in mongod's nesting — `sorted_by_index` is what decides whether a blocking `SORT` appears, which is the question a client runs `explain` to answer. The IXSCAN node carries `keyPattern` / `indexName` / `isMultiKey` / `multiKeyPaths` / `isUnique` / `isSparse` / `isPartial` / `indexVersion` / `direction` in mongod's key order; `COLLSCAN` carries `direction`; `FETCH` carries only the RESIDUAL filter and omits the key when the bounds cover the predicate. `queryPlanner.parsedQuery` and the stage `filter`s are mongod's NORMALISED match expression, built by `secantus/explain.py`'s `canonical_match` (bare equality → `$eq`, multi-clause → an `$and` sorted by mongod's match-type ordinal then path, `$ne`/`$nin` → `$not`, `$in` of one → `$eq`, `$in` of none → `$alwaysFalse`, `$all` split, `$type` → numeric codes, `$bitsAll*` → bit positions, `$comment` dropped, `$nor` kept only as the whole query). Not reproduced BY THE PYTHON SERVER: `indexBounds`, `rejectedPlans`, and the `IDHACK` / `EXPRESS_IXSCAN` / `COUNT_SCAN` / `DISTINCT_SCAN` / `PROJECTION_COVERED` executors, and its stage tree is `find`-only. Sweep: `tools/probes/explain_shapes.py`. **The Rust server does reproduce them (2026-10-12)**, because they turned out to follow fixed rules whenever only one index can serve the query: its `explain` (`crates/secantus-commands/src/explain/`) derives the plan from mongod's planning rules — bounds, residual filters, sort and covering, and the `count` / `distinct` / write stages — and derives `executionStats` by walking that plan over the documents. What it still does not reproduce is the part that really is mongod's trial of candidate plans: `rejectedPlans`, the plan cache, and the choice between several usable indexes (a fixed order of preference stands in, and agreed with mongod on every measured case). It reports the plan mongod runs, which is not always how the Rust storage router reads the documents (`tasks/backlog-rust-mongodb.md` 7.075). Sweep: `tools/probes/explain_commands.py`, 786 replies. **Since 2026-10-13 the same plan sets the ORDER the Rust server returns documents in** (`explain/order.rs`, `tasks/backlog-rust-mongodb.md` 7.080): the router says only which documents match, and the plan says in what order mongod reads them, which is also which document a `limit: 1` delete or an update without `multi` changes. A sort is still the store's own; the plan decides how documents that compare equal are left. For a sort the predicate's index does not return, the planner counts mongod's trial of the two candidate plans (`plan::walk_finishes_first`) on a collection of up to 2,000 documents. Sweep: `tools/probes/find_order.py`, 6,531 replies. When a change touches the router or the planner, run it: a router change can no longer move the order, and a planner change now can. The internal `multikey` catalog flag is stripped from `listIndexes` on the way out — mongod doesn't carry it there (re-probed 8.2.11, 2026-08-30); `Storage.index_is_multikey` is the accessor for it. Picker helpers (`_pick_compound_eq_index`, `_pick_compound_range_index`, `_find_leading_field_index`) are shared between the lookup and planning paths.

**Direction support**: single-field and compound indexes accept any per-field direction. The encoder bitwise-inverts the bytes for DESC fields so the WT B-tree gives us the index's natural order with a forward walk. Equality, `$in`, range (`$gt`/`$gte`/`$lt`/`$lte`) — operator semantics flip automatically when targeting a DESC field — and direction-aware sort acceleration all work end-to-end on single-field indexes, and the equality/prefix/trailing-operator paths all work on mixed-direction compound indexes.

**Partial indexes**: indexes accept a `partialFilterExpression` option (e.g. `{status: "active"}`); only docs that `matches()` the expression get entries written, and pickers may use a partial index only when the user query *implies* the partial filter. Implication is sound-not-complete (`_query_implies_partial` / `_op_implies_bound` / `_clause_implies_bounds`, ported to the Rust `query_implies_partial` / `op_implies_bound` / `clause_implies_bounds`): a bare equality matches an equal bare partial value, **and** the range operators `$eq`/`$lt`/`$lte`/`$gt`/`$gte` are recognised on *both* sides — `{a: {$lte: 1.5}}` is implied by a query `a: 1` or `a: {$lt: 1}`. Comparison is via `encode_value` **within one BSON type bracket only**: the range operators are type-bracketed (`{$gt: 0}` matches numbers and nothing else), so comparing across brackets — which is what a bare `encode_value` compare does, since a string sorts above every number — concluded that `{b: "x"}` implied `{b: {$gt: 0}}`, used an index that does not contain those documents, and made `find({a: 5, b: "x"})` return NOTHING (found and fixed 2026-09-01; `_type_bracket` is the gate). **NaN needs a second gate, which the bracket one cannot supply** — NaN is INSIDE the numeric bracket, but the two orders mongod maintains disagree about it: sort places it below `-Infinity` while every range operator excludes it (equality still matches it). So a sort-key compare concluded that `{b: NaN}` implied `{b: {$lte: 1.5}}`, and `find({b: NaN})` over a partial index on that filter returned NOTHING while the same query without the index returned the row — the same silent-data-loss shape as the bracket bug, one value class further in (found and fixed 2026-09-03; `_is_nan_value` refuses any non-`$eq` bound with a NaN on either side, on both servers). Document-level operators and non-range partial operators still aren't reasoned about (→ COLLSCAN, correct but slower). The picker strips partial-filter keys when matching the user filter against the index key spec, so a query like `{status: "active", n: 5}` against a partial index on `{n: 1}` with filter `{status: "active"}` correctly uses the index. A *multi-field* query against a *single-field* partial index also lights up when every residual field is exactly a partial-filter field implied by the query (`_single_field_partial_residual_match`): `find({x: {$gt: 1}, a: 1})` against an `{x:1}` index partial on `{a: {$lte: 1.5}}` rides the index on `x` while `a:1` is partial-implied (rechecked by the exact `matches()` pass). `explain` flags an IXSCAN over a partial index with `isPartial: true`.

**Sparse indexes**: a sparse index holds an entry for a doc with **at least one** of the indexed fields (`_sparse_covers`, mongod's compound-sparse rule); requiring all of them under-indexed compound sparse indexes and lost rows. **An index built before 2026-09-01 under-indexes until it is dropped and recreated** — nothing rewrites existing entries. The pickers refuse a sparse index for any query whose predicates could match a MISSING field (`_sparse_index_usable` / `_predicate_may_match_missing`: bare `null`, ANY comparison against null — `$eq` / `$lte` / `$gte` / `$lt` / `$gt`, not just `$eq`, which the first version of the gate missed — an `$in` containing null, `$exists: false`, `$ne` / `$nin` / `$not`, or an unconstrained indexed field) — a sparse index is usable only when some indexed field is *guaranteed* present. Without that gate `find({a: null})` skipped every doc missing `a`, and any collated or uncollated sort over a sparse index dropped the docs it omits. Both were silent data loss; see `tests/test_sparse_and_partial_index_fidelity.py`.

**TTL indexes**: `expireAfterSeconds` is honoured by `Storage.prune_ttl(db, coll, *, now=None)` which walks the collection, deletes docs whose indexed `datetime` field is older than `now - expireAfterSeconds`, and removes their index entries. The clock is injectable so tests can drive expiry deterministically. There is **no background sweeper** — real MongoDB prunes every 60s; SecantusDB requires the caller to invoke `prune_ttl` explicitly. Docs without the TTL field, with non-date values, or with values inside the window are left untouched.

Multi-field sort acceleration: a sort spec whose `(field, direction)` tuple list exactly matches — or fully inverts — a compound index's key spec walks the index in forward / backward order and skips the Python post-sort entirely. Picker is strict-shape only (partial-prefix sorts, mixed-direction mismatches, and multikey indexes fall back to COLLSCAN + Python sort). `_compound_index_for_sort` lives in `storage.py` next to `_single_field_index_for`; the planner mirrors the same rules in `explain_plan`.

Per-index collation shipped (strength 1/2/3 + `caseLevel` across single-field and compound indexes; `numericOrdering` still falls back to COLLSCAN — see `tasks/backlog-python-mongodb.md` §2 and `docs/indexes.md` "Per-index collation"). **Collated ORDERING is a three-level key, not a single fold** (`collation.sort_levels`, 2026-09-01): primary = base letters (accents stripped, case folded, or digit-run tuples under `numericOrdering`), secondary = each base character's combining marks weighted by a mark-order table measured from mongod (acute before grave — *not* codepoint order) and reversed under `backwards`, tertiary = case rank flipped by `caseFirst`; `strength` truncates the tuple and `caseLevel` re-adds the case rank. `ordering.sort_docs` takes a collation and `find_matching` / the `$sort` stage pass theirs. 17 of 19 sweep cases match mongod 8.2.11 (`tools/probes/collation_order.py`); the two that do not are LOCALE-specific (Swedish sorts `ä` after `z`), which is CLDR data and needs ICU. **A collated sort is never satisfied by the index walk** — the entries table holds the single-level normalisation and cannot express the levels, so `find_matching` withdraws its "already in sort order" conclusion whenever a collation is in play (the index still FETCHES; only the ordering claim is dropped). Before that, the same query returned a *different order* with and without a collated index. The collation SPEC is validated on `find` / `aggregate` / `count` / `distinct` / `findAndModify` and deliberately not on `update` / `delete`, because mongod does not validate it there either.

Out of scope regardless: text / hashed / wildcard indexes.

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

`hello` advertises the server as a single-node `secantus` replica-set primary (`setName: "secantus"`, `hosts: [<addr>]`, `secondary: false`, `primary: <addr>`, `me: <addr>`, `electionId`, `lastWrite.opTime.ts`) so pymongo's `Watch` accepts the topology. Inside a `replSetStepDown` window the same reply reports `isWritablePrimary: false` / `secondary: true` and DROPS `primary` and `electionId` — mongod's shape for a node that knows of no primary and won no election — and `topologyVersion.counter` advances, because a driver ignores a `10107` whose topologyVersion is not newer than the one it holds. Switch off via `SecantusDBServer(..., replica_set_name=None)` for tests that want a pure standalone hello reply.

Tailable cursors live in `CursorRegistry`: change-stream cursor IDs are int64-random (`> 2**32`) to dodge driver assumptions; the entry carries a `producer` closure (reads oplog → projects events), `position_seq`, `await_data`, and an `invalidated` flag. `_get_more` blocks on `Storage._oplog_cv` (a separate `Lock`-backed `Condition`, **not** the storage `RLock`) until a writer notifies via `_emit_oplog` or until the per-call timeout expires. PyMongo doesn't always send `maxTimeMS` on change-stream getMore; the server uses 1s as the default tailable wait so the connection thread can be reaped on shutdown.

Event projection lives in `secantus/changestreams.py`: `project(seq, oplog_entry, *, storage, full_document_mode, full_document_before_change_mode, scope) -> (event, invalidates)`. Resume tokens are `{"_data": "<hex>"}` where the hex is `bson.encode({"s": seq, "t": ts, "n": ns, "k": documentKey._id})` — opaque to pymongo but enough state for resume / `startAtOperationTime` / invalidation. Drops on a watched coll, dropDatabase on a watched db, and rename of a watched coll all surface a final `invalidate` event and end the cursor on the next getMore.

## The in-process engines and the parity suites

Moved from CLAUDE.md's "Engines" section (2026-10-20). Read with the
deprecation in mind: a Rust change that breaks a parity case updates the
parity EXPECTATION for that case; the Python engine is never changed to
match, and a Rust fix is never ported back.

- **Current code vs direction.** Today the code still contains the transitional
  in-process selection (`src/secantus/engine.py`: `selected()` / `enabled()` and
  the `query`/`update`/`expressions`/`projection`/`diff`/`sortkey`/`aggregate`
  shims that delegate to `_secantus_core` when enabled; `SECANTUS_ENGINE=python|
  rust|auto`). **This is being retired** in favour of the two-server model: the
  Python server reverts to pure-Python, and `_secantus_core` is kept **only as the
  parity-test vehicle**. Don't build new in-process selection surface; build the
  Rust server (`tasks/rust-server-plan.md` §4, R1–R8).
- **Parity suites stay (a DRIFT detector, not an oracle).** Each Rust engine is
  pinned byte-for-byte to its pure-Python counterpart by a
  `tests/test_rust_*_parity.py` suite (curated corpus + randomised fuzz); the
  Rust side returns a "defer to Python" signal for constructs it can't reproduce
  exactly (regex → Python `re`, collation, Decimal128 edges, non-ASCII case,
  etc.). When porting/widening a Rust engine, **extend the parity suite first**;
  never let the two engines drift.

  **But the Python engine is NOT the Rust server's exemplar** — `mongod` is (see
  "Design constraints" above). These suites say the two engines moved apart;
  they never say which one to move. A parity failure is a prompt to probe
  `mongod`, not a licence to make Rust agree with Python.
- **The parity suites do NOT cover the STORAGE layer** — they pin `query` /
  `update` / `expressions` / `projection` / `sortkey` / `diff` / `aggregate`,
  the pure engines. `secantus-storage` has its own port of the index helpers
  (`op_implies_bound`, `index_key_variants`, the pickers), and on 2026-09-01 it
  still carried four index defects — three of them silent data loss — for as long
  as it took someone to grep for them after the Python side was fixed. **When a
  fix lands in `src/secantus/storage.py`, grep `crates/secantus-storage/` for the
  same helper**: nothing else will tell you. `tools/probes/index_result_sets.py`
  is the standing cover for the index half — it compares a server against ITSELF,
  with and without the index, so it isolates the index from the sort engine and
  needs no oracle to say which side is wrong.
- **Implication for changes:** a change to an operator's semantics lands in the
  Rust port; the Python module is deprecated and is not changed to match. Keep
  the parity suite green by updating its expectation for the case the Rust
  change moved (never by changing the Python engine). The Rust server's pymongo +
  driver gauges must not regress. Plan / phase status / per-engine
  fallback lists: `tasks/rust-server-plan.md` (north star), `tasks/rust-rewrite-
  plan.md`, `tasks/rust-rewrite-phase4-scoping.md`, `tasks/rust-rewrite-spike-
  findings.md`, `tasks/backlog-rust-mongodb.md` §7. Tooling: `invoke rust-test` / `rust-build`
  / `rust-parity`.


## Detail moved from CLAUDE.md (2026-10-29)

- **Nothing publishes them.** No PyPI release since 2026-10-20: the
  `release-prepare` / `release-finalize` tasks refuse, and the PyPI
  workflows (`publish.yml`, `wheels.yml`, and `rust-wheels.yml` for the
  `secantus-core` wheel) are gone. Releases already on PyPI stay up; do not
  yank them without Joe. The only releases are the Rust ones: the
  `secantusdb-v*` and `secantusd-pg-v*` tags (binaries) and the crates.
- **CI does not test them.** The `test` job runs lint plus the files
  `tools/ci_test_selection.py` picks: every test file that does NOT import
  the `secantus` package (the ops board's tooling excepted). The Python
  servers' suites, the durable-storage and Windows lanes built for them, the
  Python-vs-Rust engine parity step, and the Python gauge lanes in
  `validate.yml` no longer run. A Python-server test that a change breaks
  is not your problem to fix; do not spend a session on it.
- **pytest stays the test runner** (Joe, 2026-10-20), for the Rust servers
  too: their Python test files (`tests/test_rust_*.py`, the PG slice and login
  differentials, `tests/backlog/`) run in the `storage-engine` and
  `pg-oracle` jobs, which build the Rust servers.
- **The `secantus` package is still load-bearing, so do not delete it.**
  Every Rust job builds WiredTiger through the package's CMake build
  (`uv build --wheel` / `uv sync`), and several Rust tests import its
  `Storage` or SQL layer (`test_rust_pgserver_slice.py`,
  `test_rust_store_handoff.py`, `test_rust_pitr_cross_server.py`). Removing
  the code is a later decision for Joe, and needs a standalone WiredTiger
  build first.
- **Compatibility with them is no longer a constraint, on-disk format
  included** (Joe, 2026-10-16: "We no longer need to worry about python
  server compatibility, we are focussed on the rust servers"). A Rust change
  may store something the Python servers cannot read, or that they would
  overwrite. Do not shape a Rust design around the Python servers, and do
  not add Python-side code to carry it. A Rust test that only checks a
  Python-server hand-off and that such a change breaks is retired with a
  written reason naming the decision, not worked around. The two RUST
  servers still share one store, and that half of the contract stands.

All four share **one on-disk format** today. Between the two Rust servers that
is a compatibility promise rather than an accident — breaking it is silent data
loss. Toward the Python servers it no longer binds (2026-10-16, above): the
cross-language bullets below record what holds now, not what must keep holding.
Be precise
about which half of it is actually pinned by a test, because they are different
claims and it is easy to cite one as evidence for the other:

- **Cross-LANGUAGE, same protocol — proven.** The Rust PG server and the Python
  PG server read and write each other's store (`tests/test_rust_pgserver_slice.py`,
  e.g. `test_python_server_reads_rust_timestamps`, which exists because getting
  it wrong is silent corruption rather than an error).
- **Cross-PROTOCOL, Python pair — proven.** A `pymongo`-written collection is
  queryable as a table, `JOIN`s and all, with no `CREATE TABLE` (`docs/sql.md`).
- **Cross-PROTOCOL, Rust pair — proven in ONE direction, and never
  concurrently** (measured 2026-09-18, pinned by three tests in
  `tests/test_rust_pgserver_slice.py`):
  - SQL → MongoDB **works**: a table written by `secantusd-pg` reads back
    through `secantusd-rs` as a collection in the database the SQL session used,
    with the `PRIMARY KEY` as `_id`.
  - MongoDB → SQL **does not**: the Rust PG server resolves names through
    `__sql_catalog__`, which only `CREATE TABLE` writes, so a `pymongo`-written
    collection is `relation "..." does not exist`. The PYTHON SQL layer samples
    an uncatalogued collection and serves it; the Rust one does not. Do not
    carry the Python behaviour across in your head — that is how this bullet
    got written wrong the first time.
  - **Not at the same time.** WiredTiger takes an exclusive file lock, so the
    second server exits rather than opening the store. "Point both at one
    directory" is a HAND-OFF, never concurrent serving — do not write it on a
    page as though both can serve at once.

### Engines: FOUR SEPARATE SERVERS across two wire protocols

**Direction (current):** SecantusDB ships **completely separate servers**, and a
user runs **one of them** — never a per-operator/per-`Storage` selection inside
one request path:

- **The Rust MongoDB server** (`secantusd-rs`, `crates/secantusdb`) — a whole,
  self-contained Rust server (its own wire / dispatch / cursors / accept loop over
  the pure-Rust `secantus-core` + `secantus-storage` + `secantus-wt` crates). No
  Python in the request path. Its Python ergonomic is a **thin embedded lifecycle
  handle** (`start`/`stop`/`address`) — the accept loop runs on a GIL-released Rust
  thread in-process and `pymongo` connects over real TCP; Python is only the
  launcher, never an operator. **This is the server the project leads with.**
- **The Rust PostgreSQL server** (`secantusd-pg`, `crates/secantus-pg`) —
  the PostgreSQL wire protocol over the same `secantus-storage`. SQL is parsed
  by **`libpg_query`** (the real PostgreSQL grammar via `pg_query`), lowered by
  `secantus-pgplan` to the MQL filters `secantus-core` already evaluates,
  against the catalog in `secantus-pgcatalog`. It builds from **its own
  directory** (`./inv rust-pgserver-build`), because it links WiredTiger and
  is excluded from the clean workspace. **Before any work on it, read the
  `rust-pg-server` skill** (`.claude/skills/rust-pg-server/SKILL.md`): how to
  build, test and measure it, the reference servers (PostgreSQL 15.19 on port
  5415, 14 as the default cluster), where each kind of change goes, and the
  rules earlier batches paid for. Open work is `tasks/backlog-rust-postgres.md`;
  the record of what landed when is git history and `changelog.d/`.
- **The Python MongoDB server** — the *original* `SecantusDBServer` (pure-Python
  `server` / `wire` / `commands` / `Storage` / operator engines). No Rust in the
  request path.
- **The Python PostgreSQL server** (`secantus.sql.pgserver`) — the PostgreSQL wire
  over the same Python `Storage`. Deprecated: the Rust PostgreSQL server is its
  replacement and now the more complete SQL surface.

## Testing the Python servers (moved from CLAUDE.md)

- The default suite runs real on-disk WiredTiger in **fast test-storage mode**
  (`SECANTUS_TEST_FAST_STORAGE=1`: no close-time checkpoint). That mode and
  `SECANTUS_FORCE_DURABLE=1` govern the PYTHON `Storage`; the CI lane that ran
  the suite durable was a Python-server lane and is gone (2026-10-20). The
  Rust servers' durability is covered by their own cargo and pytest tests.
  Details: `build-and-test`.
- **Never use a mock storage -- always the real WiredTiger-backed `Storage`**
  (`Storage(str(tmp_path))`, closed in the fixture teardown). The removed
  `FakeStorage` mock masked real bugs; do not reintroduce one.
- New CRUD operators or aggregation stages should land with both a unit test (in `tests/test_query.py` / `tests/test_update.py` / `tests/test_aggregate.py` / `tests/test_expressions.py`) and a `pymongo`-driven integration test in `tests/test_crud.py`. The integration test is the conformance proof; the unit test pins the semantics.
- Errors raised inside command handlers are caught by `dispatch` and turned into `{ok: 0, errmsg, code, codeName}`. Don't leak Python tracebacks to the wire.
