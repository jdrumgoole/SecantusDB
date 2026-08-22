### A find's first batch now respects mongod's 16MB reply cap

`getMore` already stopped at mongod's 16MB reply budget; a find or aggregate
**first** batch did not. It was capped on document count alone, so
`find` with `batchSize: 25` over 1MB documents assembled a 25MB reply and
exhausted the cursor. Measured against a real mongod 6.0.16 on the same data,
mongod returns 15 documents (15.0 MiB) and hands back a live cursor id.

Both servers now apply the same budget the cursor registry already used: stop
before the document that would overflow, but always take at least one, so a
single oversized document still makes progress rather than returning an empty
batch forever. On the Rust server the blob lengths are already known, so this
costs nothing on the hot path.

#### Fixed

- `find` / `aggregate` first batches stop under the 16MB reply cap and keep the
  cursor open for the remainder, matching mongod. Python and Rust both, covered
  by `tests/test_first_batch_byte_cap.py` and unit tests in
  `secantus-commands::find`.
