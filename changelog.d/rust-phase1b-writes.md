### Retried bulk writes, upserted ids, clustered TTL, dropped indexes and view chains on the Rust MongoDB server

Ways the Rust MongoDB server stored or returned something mongod 8.2.11 would not.

#### Fixed

- **A retried `bulkWrite` ran again.** A driver retries a write when the
  connection drops before the reply. A `$inc` was then applied twice, and
  an insert failed as a duplicate of itself. The ops that already wrote are
  now replayed, as mongod replays them: their results are rebuilt and listed
  in `retriedStmtIds`, and the rest of the batch runs. A `multi` op is
  refused with 72, because it cannot be retried. `stmtId` and `stmtIds` are
  accepted on `bulkWrite`.
- **An upserted `_id` could hold a `$`-prefixed field.** When the `_id`
  came from the query, a replacement, a pipeline or `findAndModify`, the
  server stored `{_id: {a: {$b: 1}}}`. That is a document mongod cannot make,
  and mongod refuses it with 52.
- **`upsertSupplied` took a new ObjectId** when the supplied document had no
  `_id`. It now takes the `_id` the query names by equality. A supplied
  document whose `_id` differs is refused with 66.
- **A clustered collection's `expireAfterSeconds` expired nothing.** The TTL
  pass now deletes a document whose `_id` is a date past the cutoff.
- **A cursor outlived the index it was reading.** After `dropIndexes`, a
  `find` cursor over that index went on returning documents. Its next
  `getMore` is now mongod's 175, `query plan killed :: index '<name>'
  dropped`. A cursor that scans the collection is not touched.
- **There was no limit on a chain of views.** A chain of more than 20 views
  is refused at `create` and `collMod`, and reading one of 20 is refused,
  both with mongod's 165.

`tools/probes/transactions.py` has scenarios for the retry, for two
transactions creating one collection, and for a drop under an open
transaction.
