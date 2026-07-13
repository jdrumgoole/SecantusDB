### `$meta` projection faithful error codes (both servers)

`find()`'s `{field: {$meta: <arg>}}` projection now returns the same errors real
`mongod` does. A `{$meta: "textScore"}` projection without a `$text` predicate in
the query fails with `Location40218` (`query requires text score metadata, but it
is not available`), and any unrecognized `$meta` argument fails with
`Location17308` (`Unsupported argument to $meta: <arg>`). Both errors are raised
at parse time — before matching — so they fire even against an empty collection,
matching mongod. Verified against real mongod 6.0.

For a recognized-but-unsupported `$meta` keyword (`indexKey`, `recordId`,
`sortKey`, and the search/geo/vector variants) SecantusDB degrades gracefully:
rather than emitting a wrong metadata value, it omits the projected field
entirely, leaving the rest of the projection intact. Previously the Python server
mis-handled the `$meta` value as a truthy inclusion flag and the Rust server
errored generically on it.

#### Fixed

- `projection.py` / `secantus-core` / `secantus-commands`: `{$meta: "textScore"}`
  without a `$text` query raises `Location40218`, and an unknown `$meta` argument
  raises `Location17308`, on both servers with mongod's exact codes and wording.
  A recognized-but-unsupported `$meta` arg is validated clean and the field is
  omitted from the result (partial — SecantusDB doesn't compute the metadata).
