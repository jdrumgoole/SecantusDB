### `bulkWrite` sent a 32-bit cursor id, and refused a field every 8.x driver sends

The Go driver refused every `bulkWrite` reply with
`id should be an int64 but it is a BSON 32-bit integer`. A cursor id is an
int64 on the wire; this one was a bare `0`, which BSON encodes as a 32-bit
integer. A permissive driver cannot see the difference — pymongo accepts either,
so the pymongo gauge had never noticed — but a type-strict one refuses outright.

#### Fixed

- `bulkWrite`'s reply cursor id is now `Int64`. Every other cursor reply in the
  codebase was already wrapped; this was the one that was not.
- `bulkWrite` now accepts `bypassEmptyTsReplacement`, which mongod 8.2.11 takes
  on `bulkWrite`, `insert` and `update` alike, and which the 8.x drivers append
  by default. `insert` and `update` already accepted it here; only `bulkWrite`
  refused, answering `40415 ... is an unknown field`. It is accepted and
  ignored — the flag governs a `Timestamp()` substitution neither server
  implements. A genuinely unknown field is still refused with 40415.

Both fixes apply to the Python server; the Rust server shared the
`bypassEmptyTsReplacement` refusal and already had a correctly typed cursor id,
now pinned by a test so it cannot regress. The mongo-go-driver gauge goes from
30 `bulkWrite` failures to 3, the remainder being a genuine feature gap recorded
in `tasks/backlog.md`.
