### `bulkWrite` results page through a real cursor

The reply cursor was always `{id: 0, firstBatch: [...everything...]}`, so a
driver never issued a `getMore` — three of the mongo-go-driver's CRUD prose
tests assert that it does, and the C driver fails the same three numbered cases.

#### Added

- `bulkWrite` now returns a real cursor when its results do not fit one batch,
  on both servers. `cursor.batchSize` is honoured, the remainder pages out
  through `{getMore: <id>, collection: "$cmd.bulkWrite"}` against `admin`, and
  `killCursors` closes it.
- The first batch is limited by **size** as well as count: two upserts with
  8MB `_id`s produce results that cannot share one 16MB reply, so one comes back
  and the rest follow — which is what mongod does and what a count-only rule
  misses.

Boundaries match mongod 8.2.11 exactly, including the ones that are easy to get
backwards: a batch filled *exactly* keeps no cursor (unlike `find`),
`batchSize: 0` opens a cursor with an empty first batch, and `errorsOnly` with
no errors pages nothing.
