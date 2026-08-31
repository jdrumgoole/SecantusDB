### An undefined `$$variable` is a parse error, and an empty collection proves it

`{"$project": {"x": "$$NOPE"}}` is rejected by mongod before a single document is
read — it fails the same way on a collection that is empty, and on one that does
not exist. Neither server did that, because both only discovered the problem
while evaluating the expression against a document. With no documents there was
nothing to evaluate, so both answered `ok: 1` and an empty cursor.

#### Fixed

- **An empty or non-existent collection now reports the undefined variable**
  (17276) instead of silently succeeding — on **both** servers.
- **The Rust server answers 17276 at all**, where it previously gave a generic
  `BadValue` (2) `aggregation pipeline uses a stage or operator not supported by
  the Rust server` for every one of the seven stages probed. The Python server
  had the code but, until the previous change, the wrong wrapper.
- **The wrapper is per stage**, as mongod's is: `Invalid $<stage> :: caused by ::`
  inside `$project` / `$addFields` / `$set`, and a bare message everywhere else
  (`$group`, `$redact`, `$replaceRoot`, `$replaceWith`, `$match`'s `$expr`,
  `$sortByCount`, `$bucket`).

#### How it is checked, and why conservatively

A static walk of the pipeline, run once before it executes — the only design that
can report a parse-time error, since the engine never evaluates anything when
there are no documents.

It reports **only** from positions known to hold expressions, and ignores any
stage it does not recognise. A false negative leaves the previous behaviour; a
false positive would reject a **valid** pipeline, which is far worse than the
wrong code it replaces. The rules were probed rather than assumed:

- a `$match` filter is query language, so `{"$match": {"s": "$$NOPE"}}` matches
  the literal string and must not be flagged — only its `$expr` holds an
  expression;
- `$literal`'s argument is data, not an expression;
- `$let` bindings are evaluated in the **outer** scope (they cannot see each
  other) and do not escape the `in`;
- `$map` / `$filter` bind `as`, defaulting to `this`; `$reduce` binds `this` and
  `value`;
- `$lookup`'s `let` binds only inside that stage's own sub-pipeline — naming it
  in a later stage is undefined;
- `$redact` binds `$$KEEP` / `$$PRUNE` / `$$DESCEND` for its own expression;
- `$$CLUSTER_TIME`, `$$SEARCH_META` and `$$JS_SCOPE` are *defined* variables that
  answer their own errors (10071200 / 6347902 / 51144), so they are left to those
  paths rather than reported as undefined.

#### Testing

29 cases added to `tests/test_mongod_differential.py` — 15 that must error and 14
false-positive guards that must not — plus 6 Rust unit tests. The full suite
(9405 tests, many of them valid `$let` / `$map` / `$filter` / `$reduce`
pipelines) is itself the broadest false-positive check, and stayed green.
