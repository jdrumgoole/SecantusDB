### A tailing cursor is told why its collection went away

Dropping a collection while a client is tailing it left the client with
"cursor not found" — technically true, but it doesn't say what happened, and a
tailing application can't tell a dropped collection from an expired cursor or a
server restart. MongoDB reports that the query plan was killed and names the
dropped namespace, and the Rust server now does the same.

The three kinds of cursor a drop can hit are handled differently, matching the
Python server. An ordinary cursor is discarded, so the next fetch reports the
cursor is gone. A tailing cursor is kept just long enough to explain itself.
Change streams are left alone entirely: they already announce a drop through
their own invalidation event, and turning that into an error would replace a
normal end-of-stream with a failure.

The cursors are also now killed *before* the collection is removed rather than
after — a tail parked waiting for new data is woken by the drop itself, and it
has to find the explanation already in place or it goes back to waiting on a
collection that no longer exists.

#### Fixed

- A `getMore` on a tailable cursor whose collection was dropped reports
  `QueryPlanKilled` naming the collection, instead of `CursorNotFound`.
