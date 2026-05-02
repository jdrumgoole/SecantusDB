# Architecture

SecantusDB is layered roughly outermost-in: a TCP accept loop on top of a
wire-protocol codec, on top of a command dispatch table, on top of pure
operator engines (query / update / projection / aggregation / expressions),
on top of a WiredTiger-backed storage layer.

## Layers

### `src/secantus/server.py` — `SecantusDBServer`

TCP accept loop on a daemon thread, one daemon thread per connection.
Owns the `Storage` and the `CursorRegistry`. Per-request, builds a fresh
`CommandContext(storage, cursors, db_name)` and calls into the command
dispatcher.

`port=0` lets the OS pick a free port — read it back from `server.port`
or `server.uri` after `start()`.

### `src/secantus/wire.py`

The MongoDB wire codec:

- 16-byte header (little-endian).
- `OP_MSG` (op-code 2013) parse / build — the modern op-code that
  `pymongo` 4.x uses for everything after the handshake.
- Legacy `OP_QUERY` (2004) parse + `OP_REPLY` (1) build for the initial
  handshake `pymongo` does on connect.

`OP_MSG` kind-1 document sequences are merged into the body before
dispatch, so command handlers see a single flat document.

### `src/secantus/commands.py`

Single dispatch table keyed on the first key of the request document.
Handles handshake (`hello` / `isMaster` / `ping` / `buildInfo` / ...) and
CRUD (`insert` / `find` / `update` / `delete` / `count` / `drop` /
`aggregate` / `findAndModify` / `listCollections` / ...).

Errors raised by handlers are caught and turned into
`{ok: 0, errmsg, code, codeName}`. Unknown commands return
`code: 59 CommandNotFound` so the connection survives.

### `src/secantus/query.py` — `matches(doc, filter, vars=None)`

Pure document-vs-filter predicate. Field-level operators: `$eq`, `$ne`,
`$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$nin`, `$exists`, `$not`,
`$regex` + `$options`, `$type`, `$size`, `$all`, `$mod`, `$elemMatch`.
Document-level operators: `$and`, `$or`, `$nor`, `$expr`, `$jsonSchema`,
`$comment` (no-op). Dotted paths walk into both maps and arrays.

### `src/secantus/projection.py` — `apply_projection(doc, spec)`

`find()`'s projection argument: inclusion / exclusion modes, `_id`
defaults, dotted paths, plus the `$elemMatch` projection operator that
returns the first array element matching a sub-filter.

### `src/secantus/update.py` — `apply_update(doc, update)`

Operators: `$set`, `$unset`, `$inc`, `$mul`, `$min`, `$max`, `$push`,
`$pull`, `$addToSet`, `$pop`, `$rename`. Replacement-style updates
preserve `_id`. Mixing operators with replacement fields is rejected.

### `src/secantus/expressions.py` — `evaluate(expr, doc, vars=None)`

The aggregation expression language: field paths (`"$x.y"`), `$$varname`
user vars + `$$ROOT` / `$$CURRENT`, `$literal`, arithmetic, comparison,
logical, `$cond` / `$ifNull`, `$size`, dates (with timezone support),
strings, arrays, conversions, `$mergeObjects`, ... see
[Aggregation](aggregation.md) for the full operator list.

### `src/secantus/aggregate.py` — `apply_pipeline(docs, pipeline, ctx)`

Pipeline stages — `$match`, `$count`, `$limit`, `$skip`, `$sort`,
`$project`, `$addFields` / `$set`, `$unset`, `$unwind`, `$densify`,
`$replaceRoot` / `$replaceWith`, `$group`, `$lookup` (hash-join),
`$sample`, `$sortByCount`, `$facet`, `$bucket`, `$merge`, `$out`. See
[Aggregation](aggregation.md) for details.

### `src/secantus/cursors.py` — `CursorRegistry`

Per-server, thread-safe map of int64 cursor id → remaining docs. Used
by `find` and `aggregate` to support pagination via `getMore` /
`killCursors`. Cursors carry a `last_access` timestamp; entries idle
longer than `idle_ttl_seconds` (default 600s, matching MongoDB's
10-minute cursor TTL) are pruned opportunistically. The clock is
injectable for deterministic testing.

### `src/secantus/sortkey.py`

Pure `encode_value(v)` and `encode_compound([v1, v2, ...])` that produce
**byte-sortable** bytes whose lexicographic order matches MongoDB's BSON
cross-type sort order.

Layout: `<rank_byte><payload>`. Numbers go through a "lexical decimal"
form (sign byte + bias-shifted exponent + paired BCD digits + terminator)
so int / long / double / Decimal128 collide on equal value and order
correctly across the unified numeric type. NaN / ±Infinity get dedicated
bracketing markers.

`encode_value_directed(v, direction)` bitwise-inverts the bytes when
`direction == -1` so the same encoder drives descending indexes.

### `src/secantus/storage.py` — `Storage`

WiredTiger-backed store (the same engine MongoDB itself uses). Four
tables in one WT connection:

| Table | Key format | Value | Purpose |
| --- | --- | --- | --- |
| `secantus_collections` | `SS` | BSON options blob | `(db, coll)` registry |
| `secantus_documents` | `SSu` | `bson.encode(doc)` | document store, keyed by `(db, coll, id_key)` |
| `secantus_indexes` | `SSS` | BSON `{key, options}` | index registry |
| `secantus_index_entries` | `SSSu` | `b""` | sortable index entries `(db, coll, name, packed)` |

`id_key` is `sortkey.encode_value(_id)`: byte-sortable across BSON types,
so iterating the doc table gives MongoDB's natural cross-type sort order.
`update_matching(multi=False)` and `find()` without `sort` walk in this
order, matching `mongod`.

For index entries, `packed = escape(sortkey) + b"\x00\x00" + id_key` —
the trailing `u` column packs both into one cell on purpose (WT
length-prefixes non-trailing `u` columns, which would break lex order).

WT sessions are thread-affine, kept in `threading.local()`; cursors per
session per table are cached and `reset()` between calls. A global
`RLock` serializes all public methods so we never have to think about
WT's MVCC at the storage layer.

`:memory:` is mapped to a `tempfile.mkdtemp()` opened with
`in_memory=true` and `rmtree`-cleaned on `close()`.

## Concurrency model

- **Server:** one daemon thread per accepted connection.
- **Storage:** all public methods acquire a global `RLock`; thread-safe
  by serialization, not by fine-grained locking. Fine for test workloads.
- **Cursors:** internal `Lock`, separate from storage.
- **Tests:** must run with `port=0` and `:memory:` storage. Multiple
  `SecantusDBServer` instances coexist freely.

## Type-mapping strategy

Documents are stored as **opaque BSON blobs**. All filtering, projection,
sorting, and updates happen in Python after `bson.decode`. The storage
layer never inspects document content. This is deliberate: a `pymongo`
client cannot tell SecantusDB apart from `mongod` for the operations it
supports, and any lossy intermediate representation (JSON, native column
types, etc.) would break that for `ObjectId` / `Decimal128` /
`int32`-vs-`int64` / `Date`-with-tz / `Binary` / `Regex`.

Secondary indexes are typed **sort-key columns** derived from BSON
values via `sortkey.encode_value` — not JSON, not coerced numerics.
