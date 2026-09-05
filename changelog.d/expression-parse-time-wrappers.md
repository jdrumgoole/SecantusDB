### 83 expression errors carried the wrong wrapper, with the right message inside

mongod has three wrappers for an expression that fails inside `$addFields` /
`$project` / `$set`: `Invalid $addFields :: caused by ::` for a **parse** error,
`Failed to optimize pipeline :: caused by ::` for a constant-fold failure, and
`Executor error during aggregate command on namespace: … :: caused by ::` for a
runtime one.

Eighty-three shapes carried the wrong one — and their message **body was
byte-identical** to mongod's, so only the prefix was wrong. Nothing that
compared error codes could see it, which is why they survived every earlier
sweep of this surface.

Seventy-six of the eighty-three were two operators. `$ifNull` and `$setEquals`
have their own arity code and their own wording, so they could not ride the
generic arity table and were reaching the *evaluator* instead of the parser.
The rest were missing required keys in a spec document (`$convert` without
`input`, `$dateDiff` without `startDate`, `$dateFromParts` without a year, and
the `n`-operator family without `n`).

**The ordering is load-bearing.** mongod reports an *unrecognised* key before a
*missing* required one, so `{$firstN: {k: 1}}` is "Unknown argument for 'n'
operator: k" and only `{$firstN: {}}` is "Missing value for 'n'". A first
version of this change ran the missing-key checks first, which fixed the
wrapper on all 83 and silently changed the CODE on six shapes that were already
correct. Both checks fire on the same document, so only their order separates
them; `tests/test_expression_parse_time_wrappers.py` pins it.

Against mongod 8.2.11 the probe goes from 142 message differences to **59**,
with the wrong-code set byte-identical to before.

The Rust server has the same defect and worse — it never emits the stage
wrapper at all. Filed in `tasks/backlog.md` §7 rather than guessed at, since a
fresh worktree cannot build its binary.

#### Fixed

- `aggregate.py`: `_expression_shape_problem` recognises the per-operator
  minimums (`$ifNull`, `$setEquals`) and the required-key specs, so they are
  classified as parse errors and take the stage's wrapper. Both mongod wordings
  are reproduced verbatim, including the comma `$ifNull` has and `$setEquals`
  does not.

#### Changed

- `tools/probes/agg_expressions.py`, `findandmodify_shapes.py`,
  `update_operators.py`: a `__main__` guard, so the corpus can be imported by a
  focused harness instead of the import running the whole sweep.
- `tests/test_expression_parse_time_wrappers.py` (new): 19 cases covering the
  classification, the unknown-before-missing ordering, and valid specs that must
  fall through to folding.
