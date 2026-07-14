### Unrecognized aggregation-expression operators report mongod's error codes

When a query or pipeline references an aggregation-expression operator that
doesn't exist (e.g. a typo like `$notreal`, or an operator MongoDB itself hasn't
shipped), SecantusDB now rejects it with the same context-specific error code and
message that real `mongod` returns, instead of a generic one.

An unknown operator inside a query `$expr` — `find({$expr: {$notreal: [...]}})` —
now surfaces `168 InvalidPipelineOperator` with the message
`Unrecognized expression '$notreal'` on both the Python and the Rust server
(previously the Python server returned `14 TypeMismatch` and the Rust server a
generic `2 BadValue`). An unknown operator inside an aggregation `$project` —
`aggregate([{$project: {y: {$notreal: [...]}}}])` — returns
`Location31325` `Invalid $project :: caused by :: Unknown expression $notreal` on
the Python server. mongod emits these same "unknown expression" errors even for
operators it recognises by name but hasn't implemented, so SecantusDB simply
matches that behaviour for any operator it doesn't recognise.

#### Fixed

- Query `$expr` with an unrecognized expression operator returns
  `168 InvalidPipelineOperator "Unrecognized expression '$op'"` on both servers
  (was `14 TypeMismatch` on Python, `2 BadValue` on Rust).
- Aggregation `$project` with an unrecognized expression operator returns
  `Location31325 "Invalid $project :: caused by :: Unknown expression $op"` on the
  Python server (was `14 TypeMismatch`). The Rust server still returns a generic
  `BadValue` here — faithful `$project` detection needs to distinguish the
  projection-only operators (`$slice` / `$elemMatch` / `$meta`) from expressions,
  tracked in `tasks/backlog.md` §7.
