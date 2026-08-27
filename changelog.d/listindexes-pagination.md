### `listIndexes` can be paginated again

A `listIndexes` cursor was registered under a `db.$cmd.listIndexes.<coll>`
pseudo-namespace, but drivers put the plain collection name in the follow-up
`getMore`'s `collection` field. The `getMore` ownership check — which compares
the caller's claimed namespace against the cursor's stored one, and is right to
exist — therefore rejected every continuation with `CursorNotFound`. In practice
any collection with more indexes than the batch size could not have its index
list read to the end.

Real mongod reports that cursor under the plain `db.coll` namespace. Note this
is not a blanket "drop the `$cmd` prefix": probed on mongod 8.3.4,
`listCollections` really is `db.$cmd.listCollections` and the collectionless
`aggregate: 1` form really is `db.$cmd.aggregate`. Both were already correct;
only `listIndexes` was wrong.

#### Fixed

- `listIndexes` cursors are registered and reported under `db.coll`, so a
  `getMore` continuation succeeds and every index is returned. Fixed on both the
  Python and Rust servers.
- The `getMore` cross-namespace ownership check is unchanged — a continuation
  claiming a different collection is still rejected with `CursorNotFound`.
