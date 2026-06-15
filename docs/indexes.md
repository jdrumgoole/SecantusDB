# Indexes

SecantusDB has a real query planner with index acceleration across most of
the shapes `pymongo` produces. This page documents what's wired up and how
to verify it.

## Creating indexes

Standard `pymongo` API:

```python
coll.create_index("email", unique=True)
coll.create_index([("a", 1), ("b", -1)], name="ab_compound")
coll.create_index("status", partialFilterExpression={"active": True})
coll.create_index("createdAt", expireAfterSeconds=3600)
```

The `_id` index always exists — it's the document table itself, walked by
WT-key order.

## What `find()` accelerates

`find()` routes through the index entries table for:

### Single-field filters

Bare equality (`{field: v}`), `$eq`, `$in`, and any combination of `$gt` /
`$gte` / `$lt` / `$lte` against a single-field index.

```python
coll.create_index("score")
coll.find({"score": {"$gte": 80, "$lt": 100}})  # IXSCAN on score
```

When no single-field index covers `field`, a compound index whose **leading
field** is `field` is used instead. Equality lookups become prefix scans
(`enc(v) + COMPOUND_SEP`); range bounds are evaluated with a
leading-field-only scan that uses `startswith(esc_X + esc_compound_sep)` to
identify boundary rows.

### Multi-field bare-equality filters

When the filter's fields are a leading prefix (set-wise) of an ASC compound
index, an exact match (filter covers the whole index) or a prefix scan
(strict leading prefix) runs.

Filter field order doesn't matter — `{b: 20, a: 1}` finds the same
`{a: 1, b: 1}` index as `{a: 1, b: 20}`.

### Compound prefix + trailing operator

`{a: 5, b: 10, c: {$gt: 20}}` walks a compound `{a, b, c}` index by pinning
the prefix from the equalities and applying the operator's bounds to the
next column. Supports `$eq` / `$in` / `$gt` / `$gte` / `$lt` / `$lte` on the
trailing field.

### Mixed-direction compound indexes

Compound indexes accept any per-field direction (`{a: 1, b: -1}`,
`{a: -1, b: -1}`, etc.). Each field is byte-encoded with
`encode_value_directed(value, dir)` so the entries table sorts in the
index's natural order. When the trailing field is DESC, the operator
semantics flip (`$gt` becomes upper-exclusive in byte order).

### Partial indexes

`partialFilterExpression` is honoured at write time (only matching docs get
entries) and at query time:

```python
coll.create_index("n", partialFilterExpression={"status": "active"})

# Uses the index — query implies the partial filter.
coll.find({"status": "active", "n": 5})

# Falls back to scan — query doesn't imply the partial filter.
coll.find({"n": 5})
```

The picker requires every key/value in the partial filter to appear with the
same bare value in the user filter. Partial-filter keys are stripped before
matching against the index key spec, so a query like `{status: "active",
n: 5}` against a partial `{n: 1}` index with filter `{status: "active"}`
correctly uses the index.

Conservative: operator-form clauses or document-level operators (`$or`,
`$expr`, ...) in the partial filter aren't recognised as implied.

### Sparse indexes and `$exists`

`sparse=True` is honoured at write time: a doc that is **missing** the
indexed field contributes no entry. A field that is present but `null`
still gets one — sparse keys on field *presence*, not value, matching
`mongod`.

```python
coll.create_index("phone", sparse=True)   # only docs with a `phone` get an entry
```

The semantics that matter at query time — and where the engines diverge:

- **MongoDB** can serve `{field: {$exists: true}}` from a **sparse** index
  (or, equivalently in modern MongoDB, a partial index whose filter is
  `{field: {$exists: true}}`) at IXSCAN — the sparse index *is* exactly the
  set of docs that have the field. A **non-sparse** index, or no index at
  all, forces a COLLSCAN for `$exists: true`: a normal index carries an
  entry for every doc, so it can't separate "has the field" from "doesn't".
  `{field: {$exists: false}}` **never** uses a sparse index (the matching
  docs are precisely the ones a sparse index omits) — always a COLLSCAN.
- **Other DocumentDB-compatible engines** (Amazon DocumentDB and similar)
  are stricter: a sparse index is the *only* way to get index acceleration
  for `$exists: true`, and `$exists` belongs to a family of operators
  (`$ne` / `$nin` / `$nor` / `$not` / `$exists` / `$elemMatch`) that
  otherwise never use an index at all. Drop the `$exists` clause and the
  sparse index goes unused even for queries on the same field.

**SecantusDB:** a `{field: {$exists: true}}` query rides a sparse
single-field index on `field` at IXSCAN when one exists — the planner walks
the whole index (no value bound), since the sparse index holds an entry for
exactly the docs where the field is present. A **non-sparse** index, or
`{field: {$exists: false}}`, correctly stays on COLLSCAN, matching MongoDB.
`explain` reports the `IXSCAN` accordingly.

```python
coll.create_index("phone", sparse=True)
coll.find({"phone": {"$exists": True}}).explain()  # IXSCAN on phone_1
```

> Background: Franck Pachot, ["`$exists` and non-sparse indexes in MongoDB
> and in other DocumentDB"](https://dev.to/franckpachot/exists-and-non-sparse-indexes-in-mongodb-and-in-other-documentdb-19e3).

### Multikey fallback

SecantusDB doesn't yet support per-element multikey indexing. Instead,
indexes are flagged `multikey: True` (sticky — never cleared) at insert /
update / `create_index` time when any indexed field on a doc is a list, and
the picker skips multikey-flagged indexes — `find()` falls back to a full
scan + `matches()` so array-element queries (`{tags: "python"}` against
`{tags: ["python", "go"]}`) return the correct rows.

## Sort acceleration

If the sort field matches an index's leading field, the post-sort step is
skipped — `find()` walks the index in order:

```python
coll.create_index([("createdAt", -1)])
list(coll.find().sort("createdAt", -1))   # forward walk of DESC index
list(coll.find().sort("createdAt", 1))    # backward walk of DESC index
```

Single-field sort can use any matching index — single-field or compound,
ASC or DESC. Multi-field sort still falls back to in-memory `sort_docs`
(today only single-field sort is index-accelerated).

## Hints

`hint` is honoured on both `find` and `aggregate`:

| Hint value | Behaviour |
| --- | --- |
| Index name string | Walk that index |
| Key-spec dict matching an index | Walk that index |
| `"$natural"` | Force collection scan |
| `"_id_"` / `{_id: 1}` | Walk doc table order |

An unknown hint surfaces as a `BadValue` (code 2) error to the client.
The hint can also align with the sort spec to skip the post-sort step.

`aggregate` lifts a leading `$match` stage into the initial fetch's filter
so a pipeline starting with `[{$match: {...}}]` benefits from the same
index acceleration as `find`.

## TTL indexes

`expireAfterSeconds` is honoured by `Storage.prune_ttl(db, coll, *, now=None)`
which walks the collection, deletes docs whose indexed `datetime` field is
older than `now - expireAfterSeconds`, and removes their index entries.

The clock is injectable so tests can drive expiry deterministically. There
is **no background sweeper** — real MongoDB prunes every 60s; SecantusDB
requires the caller to invoke `prune_ttl` explicitly. This is the right
ergonomics for a test harness: the test that wants TTL behaviour fires the
prune itself.

```python
from secantus import SecantusDBServer
import datetime as dt

with SecantusDBServer(port=0, storage_path=":memory:") as server:
    client = MongoClient(server.uri)
    coll = client["db"]["events"]
    coll.create_index("createdAt", expireAfterSeconds=60)
    coll.insert_one({"createdAt": dt.datetime(2026, 1, 1, tzinfo=dt.UTC)})

    # Force expiry from the test:
    pruned = server.storage.prune_ttl(
        "db", "events",
        now=dt.datetime(2026, 1, 1, 0, 5, tzinfo=dt.UTC),
    )
    assert pruned == 1
```

Docs without the TTL field, with non-date values, or with values inside the
window are left untouched.

## explain

`explain` reports `IXSCAN` when an index would be used and `COLLSCAN`
otherwise:

```python
plan = coll.find({"n": 5}).explain()["queryPlanner"]["winningPlan"]
# IXSCAN: {"stage": "FETCH", "filter": ...,
#          "inputStage": {"stage": "IXSCAN", "indexName": "n_1",
#                         "keyPattern": {"n": 1}, "direction": "forward"}}
# COLLSCAN: {"stage": "COLLSCAN", "filter": ...}
```

`Storage.explain_plan(...)` mirrors `find_matching`'s routing decisions
without executing them and is exposed on the public storage API.

## Geospatial — `2d` and `2dsphere`

Both index types ship and accelerate `$geoWithin` / `$geoIntersects`
/ `$near` / `$nearSphere` plus the `$geoNear` aggregation stage.
See the dedicated [Geospatial](geospatial.md) page for the
operator-by-operator reference, doc-side shapes accepted,
distance-unit conventions across the GeoJSON / legacy-planar /
legacy-spherical spec forms, and the worked deployment example.

Quick shapes:

```python
coll.create_index([("loc", "2dsphere")])               # GeoJSON, spherical
coll.create_index([("loc", "2d")])                     # legacy [x, y] pairs, planar
coll.create_index([("loc", "2dsphere"), ("cat", 1)])   # compound geo + scalar
```

## Natural iteration order

Walking the doc table in WT-key order yields docs in MongoDB's natural
sort order: numeric for int/float/Decimal128 (with cross-type collision
preserved by the lexical-decimal encoding), chronological for `ObjectId`,
lexical for strings, etc. `update_matching(multi=False)` and `find()`
without `sort` walk in this order, matching `mongod`.

## Index acceleration paths summary

| Filter shape | Path |
| --- | --- |
| `{f: v}` (bare-equality, single field) | Single-field index, prefix scan on compound, or partial-index match |
| `{f: {$eq: v}}` / `{f: {$in: [...]}}` | Same as bare-equality |
| `{f: {$gt/$gte/$lt/$lte: v}}` | Range scan |
| `{f1: v1, f2: v2, ...}` (bare-eq) | Compound prefix exact / scan |
| `{f1: v1, f2: {$gt: v}}` | Compound prefix + trailing operator |
| `{f: array}` against a `multikey` index | Falls back to scan |
| `{partial_filter_keys, ...index_keys}` | Partial-index path |
| Sort `{f: ±1}` aligned with an index leading field | B-tree walk in (reversed) order, no post-sort |
| `hint` | Forces a specific index / `$natural` |

## Acceleration summary across index types

| Index type | Filter / sort shape | Path |
|---|---|---|
| Single-field B-tree | equality / `$in` / `$gt`/`$gte`/`$lt`/`$lte` / sort | IXSCAN |
| Compound B-tree | bare-eq prefix; eq prefix + trailing operator on the next column; ASC/DESC mix; multi-field sort that matches or exactly inverts the key spec | IXSCAN, no post-sort |
| Partial | when the user filter implies the partial expression | IXSCAN |
| Multikey | equality / `$in` / range on the array column; whole-array equality goes through the canonical key entry | IXSCAN (sort-acceleration skipped — multikey doesn't preserve a single natural order) |
| TTL | timestamp range + prune sweeper drives expiry | IXSCAN |
| `2dsphere` | `$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere` / `$geoNear` via S2 cell-covering scan | IXSCAN — see [Geospatial](geospatial.md) |
| `2d` | same operators via quadtree-decomposed Z-order range scan over a bit-interleaved geohash | IXSCAN — see [Geospatial](geospatial.md) |
| Compound geo + scalar | geo column drives the cell scan; trailing scalar(s) filtered at the verifier step | IXSCAN |

## Per-index collation

A collation set at `createIndexes` time normalises string entries
before they're written to the entries table — strings that
collation-equal each other land at the same byte key, so a query
carrying a matching `collation` hits the same row. Strength 1/2/3
and `caseLevel` are supported; `numericOrdering` is not (would
need a length-prefixed digit-run encoding to stay byte-sortable —
queries combining it with an index fall back to COLLSCAN).

```python
coll.create_index("name", collation={"locale": "en", "strength": 2})

# Case-insensitive equality lights up at IXSCAN.
coll.find({"name": "alice"}, collation={"locale": "en", "strength": 2})
# Same collation in both → IXSCAN; mismatch → COLLSCAN.
```

Two collation rules:

* **Indexes are gated by exact-match.** An index with
  `collation: {strength: 2}` is only picked when the query's
  collation parses to the same dataclass. A no-collation query
  against a strength-2 index → COLLSCAN. A strength-3 query
  against a strength-2 index → COLLSCAN. Mongod follows the same
  rule.
* **Multiple indexes on the same field, different collations** are
  fine — the picker walks every index and uses the one whose
  collation matches. Useful for collections that mix
  case-sensitive and case-insensitive lookups against the same
  column.

A `unique` index with a collation enforces uniqueness *under* the
collation: two docs with `name: "Alice"` and `name: "alice"`
collide against a `strength: 2` unique index.

Both single-field and compound pickers thread collation through:

```python
coll.create_index([("a", 1), ("b", 1)], collation={"locale": "en", "strength": 2})

# Compound bare-equality on the full key (or any leading prefix).
coll.find({"a": "alice", "b": "BOSTON"}, collation={"locale": "en", "strength": 2})

# Compound prefix + trailing operator on the next field
# ($gt / $gte / $lt / $lte / $in / $eq).
coll.find({"a": "alice", "b": {"$gt": "k"}}, collation={"locale": "en", "strength": 2})
```

Same exact-collation-match gate as the single-field path — a
no-collation query against a collation-having compound index, or a
collation-mismatched query, falls back to COLLSCAN. Unique
compound indexes with a collation enforce uniqueness under the
collation (two compound keys that compare-equal under the
collation collide on the second insert).

Sort acceleration honours the same gate. A sort on a string field
whose only matching index has a stored `collation` walks the index
in order when the query carries a matching `collation`, and falls
back to a Python sort otherwise (the index's byte order is
collation-normalised, so walking it for a codepoint-sort query
would give the wrong order). Both single-field sorts and
multi-field sorts that exactly match (or fully invert) a compound
collation index's key spec are accelerated:

```python
coll.create_index("name", collation={"locale": "en", "strength": 2})

# Walks the index forward — explain reports IXSCAN, direction=forward.
list(coll.find().sort("name", 1).collation({"locale": "en", "strength": 2}))

# Walks the same index backward — direction=backward.
list(coll.find().sort("name", -1).collation({"locale": "en", "strength": 2}))

# Sort without a matching collation falls back to Python sort.
list(coll.find().sort("name", 1))   # COLLSCAN
```

## What's still missing

- **TTL background sweeper** — `prune_ttl` is opt-in; no 60-second
  cadence sweeper. Real mongod runs one; for an in-process test
  surrogate the explicit-call ergonomics suit the audience better.
- **Text / hashed indexes** — out of scope (no full-text engine; no
  practical workload pulling hashed shard-key behaviour into an
  in-process surrogate).
