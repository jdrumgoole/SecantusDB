### A `$switch` that can never match is now rejected, not silently ignored

An aggregation whose `$switch` had no matching branch and no `default` was
supposed to fail. When every one of its cases was a constant — `{case: false}`,
or `{$gt: [1, 99]}` — MongoDB rejects the pipeline while optimizing it, before
a single document is read, so the error arrives even over an empty collection.
SecantusDB instead waited until execution found a document to fail on. Over an
empty collection there was none, so the pipeline reported success and returned
an empty cursor: a query that could not possibly have done what was asked came
back looking like it had. Adding one document to the same collection surfaced
the error, which made the behaviour depend on the data rather than the query.

`$replaceRoot` was a second case of the same kind of drift. When `newRoot`
evaluated to something other than a document it reported the generic type
error, code 14, with a message no real server emits. MongoDB answers a specific
code — 40228 — and a message that names the offending value, its BSON type and
the input document that produced it. That last part is not the stored document
but the *pruned* one, narrowed to just the fields the expression reads, which is
what MongoDB's dependency analysis hands the stage; `$replaceWith` shares the
shape but names a different subject.

All of it was found by running the operations against a live MongoDB 8.2.11 and
comparing the replies verbatim, and the thirty cases that pin it are now part of
the differential gate.

#### Fixed

- `$switch` with only constant cases and no `default` is rejected during
  pipeline optimization (`40069`), matching MongoDB — including over an empty
  collection, where SecantusDB previously returned an empty cursor and `ok: 1`.
  A single field-referencing case, or a `default`, correctly prevents folding.
- `$switch` that finds no matching branch at execution time now reports
  MongoDB's `40066` and its wording, rather than a generic `14 TypeMismatch`.
- `$replaceRoot` / `$replaceWith` whose expression does not evaluate to a
  document now report MongoDB's `40228` and its full message, including the
  value, its BSON type, and the dependency-pruned input document.
- `$bucket`'s out-of-range-value error (`7158303`) is now wrapped in MongoDB's
  `Executor error during aggregate command on namespace: … :: caused by ::`
  prefix, which marks it as an execution failure rather than a parse error.
