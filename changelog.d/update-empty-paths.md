### An update path may not be empty

`{$set: {"": 1}}` used to succeed on both servers and store `{"": 1}` — a
document mongod cannot produce, and one the very query that created it then
fails to match. The rule applied uniformly to ten operators: `$set`, `$unset`,
`$inc`, `$mul`, `$min`, `$max`, `$push`, `$addToSet`, `$pop` and `$bit` all
either wrote an empty field name or answered the wrong code, twenty shapes in
all. mongod rejects every one of them with `56`, and it distinguishes a wholly
empty path (`An empty update path is not valid.`) from one with an empty
component (`The update path 'a.' contains an empty field name, which is not
allowed.`).

Getting the code right also meant getting the ORDER right. mongod validates an
update spec in a single walk in document order — the operator's name, then each
of its paths for emptiness, then for a conflict with a path claimed earlier —
and reports the first offender it meets. Both servers had run those checks as
separate passes, so an unknown modifier anywhere in the spec preempted an empty
path or a conflict that mongod reports first. All three checks now share one
walk on both servers, which is also how the Rust command layer stopped keeping
its own private copy of the modifier list.

Two smaller `$each` fixes ride along, both measured the same day: `$addToSet`
with a non-array `$each` answers `14` rather than the `2` it had borrowed from
its `$push` sibling, and on the Rust server a non-array `$each` under `$push`
now reports mongod's message instead of claiming the server cannot do `$push`.

#### Fixed

- `secantus.update` / `secantus-core`: an empty update path, or a path with an
  empty component, is rejected with mongod's code 56 and its two messages,
  across every operator and both ends of a `$rename`. Replacement-style updates
  are deliberately exempt — mongod really does store an empty field name for
  `replace_one({_id: 1}, {"": 1})`.
- `secantus.update` / `secantus-core`: the unknown-modifier, empty-path and
  path-conflict checks share one document-order walk, so the first offender
  wins, as mongod's does. These are parse errors: they are reported even when
  the filter matches nothing, and they come back bare.
- `secantus.update` / `secantus-core`: `$addToSet` with a non-array `$each`
  answers `TypeMismatch` (14), not `BadValue` (2).
- `secantus-core`: `$push` with a non-array `$each` reports
  `The argument to $each in $push must be an array but it was of type: <type>`
  instead of deferring, which on the standalone Rust server surfaced as
  "query uses a construct the Rust server does not support".
