### Indexes on arrays of subdocuments now actually get used

An index on a dotted path that reaches inside an array — `{"prices.owner_id": 1}`
over documents shaped like `{prices: [{owner_id: …}, …]}` — generated no index
entries at all. The documents were stored correctly and a collection scan found
them, but the moment the index existed the query planner chose an index scan and
that scan returned nothing. Any application whose schema embeds an array of
subdocuments and indexes a field inside it would silently start missing rows as
soon as its indexes were created, which for most ODMs is at application startup.
The fix makes index-key generation walk *through* arrays the way MongoDB's does:
one key per array element's value, plus the whole-array key when the array is the
leaf itself.

The same walk now drives the multikey flag, so an index over an array-valued path
reports `isMultiKey: true` from `explain`, and uniqueness enforcement was moved
onto the full set of keys a document generates rather than a single canonical
one — mongod's rule is that two documents collide as soon as they share any
generated key, which is what a unique index over an array field has to mean.
Sparse indexes covering such a path now include those documents instead of
treating the path as missing.

The behaviour here was pinned against a real `mongod` 6.0.16 rather than assumed:
an eighteen-case differential probe (element and whole-array equality, `$in`,
ranges, compound and nested and positional paths, unique and sparse variants,
missing and empty-array paths) returns byte-identical results from both servers.
That probe also corrected an assumption in the original bug report — `listIndexes`
carries no `multiKey` field on mongod, so SecantusDB no longer emits its internal
one over the wire either.

#### Fixed

- `paths.py` / `secantus-core`: new `get_path_values` resolves a dotted path
  through arrays, returning every reachable value plus whether an array was
  traversed.
- `storage.py` / `secantus-storage`: index-key generation, the multikey flag, and
  the sparse-index gate all use that walk, so a path descending into an array is
  indexed per element.
- `storage.py` / `secantus-storage`: unique enforcement (`_unique_conflict`, the
  `createIndexes` pre-check, and `find_index_duplicates`) probes every key a
  document contributes instead of one canonical key, and a duplicate-key error
  reports the value actually behind the conflicting key.

#### Added

- `explain` reports `isMultiKey` on the `IXSCAN` stage, on both servers.

#### Changed

- `listIndexes` no longer echoes the internal `multikey` catalog flag, matching
  mongod. The admin console's multikey badge is gone with it — the flag isn't
  wire-visible, and the console reports what the wire says; `isMultiKey` in the
  explain visualiser carries the same information.
