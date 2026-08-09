### Large batch inserts no longer risk a storage livelock

A single insert message can carry up to 48MB of documents, and the Python
server used to write the entire batch — document rows, their full-document
oplog entries, and every index entry, roughly two to three times the message
bytes — inside one WiredTiger transaction. A transaction's dirty content is
unevictable, so a large enough batch could pin the storage cache past its
dirty-stall threshold and livelock the engine: every thread drafted into
eviction, nothing evictable, and only the stuck writer's own commit able to
free the cache. This is what wedged the mongo-rust-driver conformance
gauge's `large_insert` test (35,000 tweet-sized documents) in weekly CI —
and once wedged, the server never recovered.

Inserts now commit in bounded chunks of at most 1,000 documents or 4MB per
statement transaction, mirroring what real mongod does with its internal
insert batches. MongoDB batch inserts are per-document atomic only — a batch
has never been all-or-nothing — so the extra commit points are invisible to
clients: ordered batches still stop at the first error with the correct
per-document index, unordered batches still report every error, and capped
collections still never evict documents from the batch being inserted.

#### Fixed

- `secantus.storage.insert`: one wire batch no longer runs as one WiredTiger
  statement transaction; chunks are bounded at 1,000 docs / 4MB with the
  write-conflict retry scoped per chunk. Reproduced and pinned by
  `test_storage.py::test_large_batch_insert_survives_a_small_cache` (35k ×
  1.1KB documents against a deliberately small 128M cache) plus
  ordered/unordered cross-chunk semantics tests.
