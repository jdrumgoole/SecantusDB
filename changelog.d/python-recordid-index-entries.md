### Index scans on the Python server fetch documents in one hop, not two

An index entry on the Python server stored the encoded `_id` of the document it
pointed at. Since the document table became keyed by RecordId, resolving that
back to a row meant a second lookup through the `_id` index on every document an
index scan returned — a per-result cost paid by every indexed query.

Entries now carry the RecordId itself, as an eight-byte big-endian tail, so an
index scan reads its documents straight from the table. Big-endian is what keeps
the entries for a single key in insertion order, and being fixed-width it needs
no escaping even where the RecordId's own bytes look like the field separator.
This is the same entry layout the Rust server writes, so the two servers now
agree on the whole on-disk format — documents and indexes both — which is what
cross-server backup and point-in-time recovery need.

Because nothing in WiredTiger's own schema distinguishes the two entry layouts —
both are opaque bytes in the same column shape — each index records the format it
was built with in the catalog. A data directory holding indexes from an earlier
build is refused at open, naming the index, rather than being read as if its
entries pointed somewhere they do not. There is no in-place upgrade: start from a
fresh data directory, or drop and recreate the indexes with the older build.

#### Changed

- Python server: index entries store the document's RecordId (8-byte
  big-endian) instead of its encoded `_id`, removing a lookup per result from
  every index scan. Matches the Rust server's entry format byte for byte.
- Python server: uniqueness checks exclude the document being updated by
  RecordId rather than by encoded `_id`.

#### Fixed

- Python server: a store whose indexes predate this change is refused at open
  with a message naming the index and the format mismatch, instead of running
  index scans that quietly match nothing.

#### Added

- Python server: indexes record `entryFormat` in the catalog. Like the internal
  `multikey` flag it is stripped from `listIndexes`, so clients never see it.
