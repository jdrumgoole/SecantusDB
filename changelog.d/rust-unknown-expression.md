### The Rust server names an unknown expression operator

Nine of thirteen measured shapes came back as
`2 BadValue "aggregation pipeline uses a stage or operator not supported by the
Rust server"` — which told the client the server could not do `$addFields`, when
what it could not do was the unrecognised operator inside it. A `Fallback::Defer`
has no Python behind it on the standalone server, so it reaches the client as
that blanket refusal.

#### Fixed

- An unknown operator in `$addFields` / `$set` / a nested `$project` position
  now answers `168 Invalid $<stage> :: caused by :: Unrecognized expression
  '$x'`, and in `$group` / `$replaceWith` / `$match`'s `$expr` the same `168`
  with no envelope — both mongod's.
- A nested unknown inside `$project` answered `31325` (the projection parser's
  code) because the check recursed. mongod uses `31325` only for the top-level
  value of a `$project` field; anything deeper is `168`.
- `codeName` for code 168 is now `InvalidPipelineOperator` rather than
  `Location168`.

Both servers are now 0 of 13 divergent on
`tools/probes/unknown_expression_errors.py`.
