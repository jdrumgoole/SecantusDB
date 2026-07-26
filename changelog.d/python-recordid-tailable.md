### Tailable cursors follow insertion order, not `_id` order

A tailable cursor on a capped collection remembered its position by the encoded
`_id` of the last document it delivered. That works only while `_id` values
increase — the default `ObjectId` case. Give a capped collection your own
descending or otherwise unordered `_id`s and the cursor breaks in both
directions: documents inserted later but carrying a smaller `_id` sort below the
mark and never reach the client, while documents the client already received can
sort above it and be delivered a second time. Feeding `500, 400, 300` into a
capped collection and then inserting `20, 10` produced `500, 400, 300, 400, 500`
where MongoDB produces `500, 400, 300, 20, 10`.

The cursor now remembers its position by RecordId — the insertion counter the
document table is keyed by. That is the order MongoDB's tailable cursors follow,
and the order capped collections evict in, so the "has my anchor been evicted?"
check that ends a lapped cursor with `CappedPositionLost` now asks about exactly
the document eviction removes. Collections with the default `ObjectId` `_id`
behave as they always did, since for those the two orders coincide.

#### Fixed

- Python server: a tailable cursor on a capped collection with non-monotonic
  `_id` values no longer drops documents inserted after the cursor opened, and
  no longer redelivers documents it already returned.
- Python server: capped-rollover detection (`CappedPositionLost`) is based on
  the oldest-inserted document rather than the smallest `_id`, matching what
  capped eviction actually removes.
