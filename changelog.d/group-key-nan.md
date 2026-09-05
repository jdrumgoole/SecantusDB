### `$group` gave every NaN its own bucket

Two documents with `a: NaN` grouped as **two buckets of one** where mongod
reports one bucket of two. A wrong aggregation result rather than an error, so
nothing surfaced it — the kind that gets absorbed into a report and never
questioned.

Python dicts key a NaN by IDENTITY: `hash(nan)` is 0 so it *is* hashable, but
`nan != nan`, so it never matches itself on lookup. The group-key
canonicaliser returned hashable values unchanged, and every NaN became its own
key.

**Both engines were wrong, differently, and both cited the other as their
authority.** The Rust side *deferred* on NaN — safe while the pure engine is
behind it, but the standalone Rust server has no Python behind a defer, so
`$group` by a NaN could not group at all there. Its comment justified this with
"NaN never equals itself in a dict probe", and the key type's doc header
described itself as mirroring "Python dict equality". Both describe Python, not
the server.

mongod's rule is simpler than either implementation assumed (probed 8.2.11,
2026-09-05): **every NaN is one key**, a `Decimal128` NaN merges with a double
NaN into that same key, and the rule applies inside arrays and subdocuments.

Neighbours on the same key path were checked: `$sortByCount` and compound group
keys had it too and are fixed with it. `$addToSet`, `$setUnion` and sorting were
already correct, which is why nothing had surfaced this.

#### Fixed

- `aggregate.py`: `_hashable_scalar` maps every NaN — float or `Decimal128` —
  to one canonical bucket key, before the plain-hashable path that returned it
  unchanged.
- `crates/secantus-core/src/group.rs`: `GKey::Nan` replaces the defer, so the
  standalone Rust server groups a NaN instead of failing on it. A non-NaN
  `Decimal128` still defers, which is a separate gap.
