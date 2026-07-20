### Capped collections evict in true FIFO order

The Python server evicted overflowing capped-collection documents by walking
the document table in `_id` byte order, which only equals insertion order
when `_id` is monotonic (the default `ObjectId`). With user-supplied
non-monotonic `_id` values it evicted the wrong documents — the smallest
`_id` first rather than the oldest-inserted — diverging from mongod's strict
FIFO semantics.

Eviction now walks the natural-order (insertion-order) index via
`_scan_docs_natural`, so the oldest-inserted document is always evicted
first regardless of `_id` shape. The Rust server already did this. Insert
`5, 1, 9, 2, 7` into a `max: 3` capped collection and the survivors are now
`9, 2, 7` (the last three inserted), not `5, 7, 9` (the three highest `_id`).

#### Fixed

- Python server: capped-collection eviction is now strict FIFO
  (insertion order) even for non-monotonic user `_id` values, matching
  mongod, instead of evicting in `_id` byte order.
