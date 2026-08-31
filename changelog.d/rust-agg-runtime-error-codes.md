### Runtime aggregation errors on the Rust server report mongod's codes

A pipeline that fails while *processing documents* — as opposed to failing its
spec, which the command layer already validates — answered a single generic
`2 BadValue` on the Rust server, because the engine can only signal such a
failure as "defer to Python" and the Rust server has no Python to defer to.
Measured against mongod 8.2.11, six of seven probed cases were divergent.

#### Fixed

- **`$densify` over a non-numeric, non-date field** now answers
  `5733201 Densify field type must be numeric or a date`.
- **`$bucket` with a value outside every boundary and no `default`** now
  answers `7158303`. mongod implements `$bucket` over `$switch`, so it reports
  the `$switch` sentence under `$bucket`'s own code — reproduced.
- **`$switch` with no matching branch and no `default`** now answers `40066`.

All three carry mongod's executor wrapper
(`Executor error during aggregate command on namespace: <ns> :: caused by ::`).

#### Known gaps

Three cases still answer the generic message, each needing machinery this
pattern does not provide, and each recorded in `tasks/remaining-work-plan.md`:

- `$replaceRoot` with a scalar `newRoot` (40228), whose message quotes the
  input document **pruned to the fields the expression reads** — mongod runs
  dependency analysis before the stage.
- `$arrayToObject` (40386) and `$concatArrays` (28664), which are expression
  type errors rather than stage errors, and are the head of a per-operator
  long tail.

The Python server already matched mongod on all seven cases; this release
changes the Rust server only.
