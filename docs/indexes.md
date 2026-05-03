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

## What's still missing

- **Multi-field sort acceleration** — sort `{a: 1, b: 1}` matching a
  compound `{a: 1, b: 1}` index would skip the post-sort entirely; today
  only single-field sort is index-accelerated.
- **Multikey indexing** — needed for fully-correct index lookups on
  array-valued fields (currently we sticky-flag and fall back to scan).
- **Collation** — accepted as an option but ignored (Python compares
  with default locale).
- **TTL background sweeper** — `prune_ttl` is opt-in; no 60-second
  cadence sweeper.
