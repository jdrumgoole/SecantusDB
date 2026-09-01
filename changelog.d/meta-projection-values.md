### `$meta` projections return real values

`{$meta: "recordId"}`, `"sortKey"` and `"indexKey"` validated cleanly and then
produced nothing — the field was simply absent from every document. MongoDB
returns real metadata for all three, and SecantusDB has the data behind each of
them (a RecordId per row, the sort specification, the index the query chose); it
was never plumbed through to the projection.

All three now compute. `sortKey` reports the sort fields in order, taking the
same representative element for an array-valued field that the sort itself took,
and `null` for a missing one. `indexKey` reports the indexed fields' values, and
only for a real secondary-index scan — a collection scan and the `_id` fast path
both omit the field, as MongoDB does. Asking for `sortKey` without a sort is now
the error MongoDB gives rather than a silently missing field, and an unrecognised
`$meta` argument reports MongoDB's wording (`Unsupported $meta field: zzz`) in
place of our own paraphrase, which a test had been pinning.

One deliberate difference: SecantusDB's RecordId is a store-wide insertion
counter where MongoDB restarts it per collection, so the numbers differ for a
second collection in the same store. The properties a caller can use — unique
per row, ascending in insertion order, unchanged by sorting the output — hold.
Matching the numbers would mean a per-collection counter, and that counter is
the document table's key, shared byte-for-byte with the Rust server.

The metadata SecantusDB has no machinery for at all — text scoring, Atlas Search,
`$geoNear` distances — still validates and then omits its field, which is
graceful degradation rather than a wrong value.

#### Fixed

- `projection.py` / `storage.py`: `$meta` values are computed for `recordId`,
  `sortKey` and `indexKey`; `sortKey` without a sort is rejected; the unknown-
  argument message matches MongoDB.
