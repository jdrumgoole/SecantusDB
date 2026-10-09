### The Rust MongoDB server accepts `sort: {$natural: ±1}` on `find` again

Since the build of 2026-10-05, every `find().sort("$natural", ...)` against
`secantusd-rs` failed with `16410 FieldPath field names may not start with
'$'`. A sort validator shared between `find` and the aggregation `$sort` stage
applied the stage's rule to both, and only the stage reads `$natural` as a
field path. The driver-gauge refresh caught it: pymongo's
`test_to_list_tailable`, which reads the newest oplog entry this way, failed on
the Rust server in both the sync and async suites.

Measured against mongod 8.2.11 with the same commands on both servers: 38
cases, no differences after the fix.

#### Fixed

- `find` with `sort: {$natural: 1}` or `{$natural: -1}` returns documents in
  storage order, forwards or backwards, and composes with `filter`, `skip` and
  `limit`. `explain` reports it as a `COLLSCAN` with a direction.
- A `$natural` sort value other than exactly 1 or -1, or `$natural` beside
  another key, answers mongod's `2 $natural sort cannot be set to a value
  other than -1 or 1.`
- A `hint` alongside a `$natural` sort follows mongod: the same-direction
  `$natural` hint is accepted, the opposite direction and any index hint are
  refused with mongod's messages.
- `findAndModify` with `sort: {$natural: -1}` used to act on the FIRST
  document. It now acts on the last, as mongod does.
