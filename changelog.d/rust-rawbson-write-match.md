### The Rust server matches update/delete candidates over raw BSON too

Completing the raw-BSON match work: `update` and `delete` scanned their
candidate documents by decoding each one in full to run the filter, then
discarded that document for every candidate the filter rejected — the same
waste `find` and `count` already avoid. Both now match candidates over raw
BSON (`query::matches_raw`, decoding only the filter's fields) and decode the
full document only for a candidate that actually matches (which the write
needs anyway). A selective `update` / `delete` over a collection scan of wide
documents no longer decodes the rows it isn't going to touch.

On a 5000-row collection scan of wide documents (11 fields) updating the ~10
that match one unindexed filter field, this measured **~4× faster** — the
baseline decoded all 5000 candidates to update ten. This reuses the same
matcher `find` and `count` use, already pinned bool-for-bool to the owned
matcher (and pure Python) across the query parity corpus — so no query
semantics change.

#### Changed

- Rust server: `update` and `delete` candidate scans filter over raw BSON
  (`query::matches_raw`) instead of decoding every candidate; only a matched
  candidate is fully decoded (for the write / oplog). Every scan-matching
  path — `find`, `count`, `update`, `delete` — now skips decoding the
  documents a selective filter rejects.
