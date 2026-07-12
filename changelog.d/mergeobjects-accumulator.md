### `$mergeObjects` as a `$group` / `$setWindowFields` accumulator

MongoDB's `$mergeObjects` was already available as a `$project` expression, but
not as an accumulator inside `$group` or `$setWindowFields`. It now is: SecantusDB
merges each group member's operand document into a single accumulated document,
with later documents' keys overriding earlier ones. A null or missing operand is
skipped, a group whose operands are all missing/null yields an empty document
`{}`, and a non-null, non-document operand raises the same `Location40400` error
mongod returns — so `{$group: {_id: "$g", merged: {$mergeObjects: "$sub"}}}` now
behaves exactly like a real server.

The accumulator ships on both the Python server and the Rust server, pinned
byte-for-byte by the aggregation parity harness.

#### Added

- `aggregate.py` / `secantus-core` (`group.rs`): `$mergeObjects` accumulator for
  `$group` and `$setWindowFields` — merge operand documents across the group
  (later keys win), skip null/missing, empty group → `{}`, non-document operand →
  `Location40400`.
