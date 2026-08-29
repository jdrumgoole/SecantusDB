### Cursor round-trip counts now match mongod

A cursor whose result count was an exact multiple of `batchSize` closed one
`getMore` early. mongod closes a cursor only when it *knows* the result is
finished — a batch that exactly fills the requested size proves nothing about
what follows, so mongod keeps the cursor open and the client spends one more
`getMore` to see an empty batch. SecantusDB buffers the whole result and so could
close early: fewer round trips, but a count drivers observe directly.

The rule was probed against a real mongod rather than inferred, and it is **not
uniform across commands**. `find` and `aggregate` keep the cursor open on an
exact-fill batch; `listIndexes` and `listCollections` close, because they
enumerate a catalog whose size is known up front. A `find` with `limit` or
`singleBatch` is bounded again and closes without the extra trip, and a
`getMore` with no `batchSize` means "server default", where mongod drains and
closes.

#### Fixed

- `find` and `aggregate` cursors stay open past a batch that exactly drains the
  result, matching mongod's round-trip count. Verified across 12 shapes against
  mongod 8.3.4, on both the Python and Rust servers.
- `limit` / `singleBatch` still close immediately, with no trailing empty batch.
- `listIndexes` / `listCollections` still close on an exact-fill batch.
- A `getMore` with a non-positive `batchSize` drains the cursor and closes it.
