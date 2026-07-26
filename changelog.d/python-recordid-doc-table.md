### The Python server's document table is keyed by RecordId, like the Rust one

Every insert on the Python server used to write four rows: the document, its
index entries, and *two* natural-order rows mapping an insertion sequence to the
document and back again. That second pair existed because the document table was
keyed by the encoded `_id`, so the order documents came back in was `_id` order,
not the order they were inserted — and MongoDB returns an unsorted `find()` in
insertion order. The side index papered over the difference at the cost of an
extra write on every insert and an extra lookup on every scan.

The document table is now keyed by the RecordId — a monotonic insertion counter —
so walking it *is* insertion order and the forward side index is gone: three
writes per insert instead of four. The document's `_id` key travels inside the row
value in a length-prefixed frame, and the remaining reverse index (`_id` →
RecordId) is now a real `_id` index, which is also where a duplicate `_id` is
caught. This is the same on-disk scheme the Rust server has shipped since its
RecordId work, byte for byte, so a store written by one server reads on the other
— the property that cross-server backup and point-in-time recovery depend on.

Measured against the previous build on the same machine — this change together
with the two that follow it — inserting ten thousand documents got about 15%
faster, an unsorted scan of them about 35% faster, and a `$group` aggregation
about 24% faster. The scan and aggregate wins are the removed side-index hop;
the insert win is the removed write. An indexed range query is about 4% slower:
it was already a direct lookup before, and it now pays the small cost of
unpacking the document's key out of the row.

One visible behaviour change comes with it. An unindexed `update` with
`multi: false` modifies the *first-inserted* matching document, as mongod does,
rather than the one with the smallest `_id`; the two only ever agreed when `_id`
values were monotonic.

Because the on-disk layout changed, a data directory written by an earlier build
is refused at open with an explicit error rather than silently mis-read. There is
no in-place upgrade: start from a fresh data directory, or downgrade to the build
that wrote it.

#### Changed

- Python server: the documents table is keyed `(db, collection, RecordId)` with
  a framed `[u32-LE id_key_len][id_key][blob]` value, matching the Rust server's
  on-disk format exactly. Insert write-amplification drops from four rows to
  three, and an unsorted scan no longer walks a side index to find insertion
  order.
- Python server: an unindexed `update`/`delete` candidate scan now visits
  documents in insertion order rather than `_id` order, so `multi: false` picks
  the oldest matching document like mongod.

#### Removed

- Python server: the forward natural-order table (`secantus_natural`) is no
  longer written. The reverse table (`secantus_natural_seq`) remains as the
  `_id` index.

#### Fixed

- Python server: a store written before this change is now refused at open with
  a clear message naming the format mismatch, instead of being opened and read
  with the wrong key format.
