### `maxTimeMS` is checked on every command, not just `find`

`maxTimeMS` is a generic command field — MongoDB's IDL validates it on
everything from `find` to `ping` — but SecantusDB checked it in `find` alone.
On the other twenty-three commands that accept it, a wrong-typed value was
taken without complaint: `db.command({"aggregate": "c", "pipeline": [],
"cursor": {}, "maxTimeMS": "x"})` ran the pipeline and reported success. That is
the silently-accepted failure mode, where a driver bug reaches the application
as a correct-looking answer. The check now runs once in `dispatch`, beside the
`readConcern` and `apiVersion` checks, so every command gets it.

The four behaviours it checked were also still the MongoDB 6.0 ones, and 8.x
honours none of them. A wrong type is now a `TypeMismatch` (14) naming the IDL
struct, not a `BadValue` (2); a fractional value is `FailedToParse` (9); the
negative and out-of-range messages changed wording and gained an upper bound of
2147483647; and an explicit `null` is now accepted, meaning the field was not
sent. The old code's own docstring described the slot as "the only one in this
sweep that is not a TypeMismatch", which is exactly what stopped being true.

Three of the rules would have been wrong if reasoned about rather than
measured, so all of them were probed across 24 commands against a live
`mongod` 8.2.1 — 436 cases, matching exactly. The IDL struct name in the type
error is the command's own name for all twenty-four *except* `find`, which
reports `FindCommandRequest`. The check order matters: `-1.5` is both
non-integral and negative, and MongoDB answers the integral error rather than
the range one. And a fractional `Decimal128` gets different wording from a
fractional `double` for the same numeric value. Where the check sits in dispatch
was measured too — `CommandNotFound` takes precedence over it, while it takes
precedence over the authorization check.

Separately, `createIndexes` with an explicit `indexes: null` answered MongoDB
6.0's `10065` where 8.x treats an explicit null as the field being absent and
answers `40414`, identical to omitting it — the same null-means-absent rule
already applied to `findAndModify`'s `arrayFilters` and `killCursors`'
`cursors`.

Both fixes land on **both servers**. The Rust server had ported the same 6.0
contract, comment and all, and called it from three commands rather than one —
so it shared the bug in a slightly milder form. Neither the engine-parity suites
nor the driver gauges would have caught the drift, because this is command-layer
behaviour that no parity suite covers.

#### Fixed

- `maxTimeMS` is validated on every command that accepts it, not only `find`;
  `aggregate`, `count`, `distinct`, `insert`, `update`, `delete` and 17 others
  previously accepted a wrong-typed value silently.
- `maxTimeMS` follows the MongoDB 8.x contract: `TypeMismatch` (14) with the IDL
  struct name for a wrong type, `FailedToParse` (9) for a non-integral or
  unrepresentable number, `BadValue` (2) for a value below 0 or above
  2147483647, and an explicit `null` accepted as absent.
- `createIndexes` with `indexes: null` answers `40414 IDLFailedToParse`, the
  same reply as omitting the field, instead of `10065`.
- The Rust server gets both fixes, having carried the same MongoDB 6.0 contract
  and validated `maxTimeMS` on three commands rather than all of them.
