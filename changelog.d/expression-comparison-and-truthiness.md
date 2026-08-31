### The expression language's comparisons and truthiness, measured operator by operator

The first sweep of the aggregation **expression** operators —
`tools/probes/agg_expressions.py`, 143 operators against mongod 8.2.11. The
argument, change-stream, update and findAndModify surfaces had all been swept;
this one, the largest operator family in the server, never had.

It found four defects that produce **silent wrong answers**, all on both servers.

#### Fixed

- **Every cross-type comparison answered false.** `$gt` / `$gte` / `$lt` / `$lte`
  compared with the host language's own operators and swallowed the error a
  cross-type pair raises:

      try:
          return bool(a > b)
      except TypeError:
          return False

  So `{$gt: ["abc", 1]}` was false where mongod says true — a string sorts after
  a number in BSON's canonical order — and `{$lt: [null, 1]}` likewise. `$cmp`,
  two thousand lines away, had used the correct comparator all along. The four
  now share it. This reaches rows: the expression language drives `$expr`,
  `$cond`, `$filter`, `$switch` and `$bucket`.
- **`$and` / `$or` iterated a non-array argument.** `{$and: "$s"}` is a
  one-element list to mongod; iterating it directly walked the string character
  by character, so `"$s"` became `'$'` and `'s'` and the first parsed as an empty
  field path. Short-circuiting is preserved — mongod does short-circuit at
  runtime, so a false `$and` operand hides a later error.
- **Truthiness.** Only null, missing, `false` and zero are false. Every string is
  true, the **empty one included** (`{$or: ""}`, `{$toBool: ""}`), as are empty
  arrays and documents. Missing was reading as true.
- **A bool is not a number.** `{$eq: [true, 1]}` is false on mongod; the host
  language's `True == 1` made it true. The same root cause was collapsing
  `$addToSet`'s `0` and `false` into one element, making `{$in: [false, [0]]}`
  true, and — in the oplog **update-diff** — reporting a field that changed from
  `true` to `1` as no change at all, so a change stream never saw it.

#### Also fixed, uncovered by the above

- **`decimal.Decimal` was not ranked as a number** in the BSON order. The SQL
  layer's numerics are native Decimals, so once the relational operators started
  using that order, `price < cost * 1.5` compared by TYPE and returned rows where
  the comparison is false. Caught by the SQL suite.
- **Booleans were excluded from the Rust engine's sortable set**, so every
  comparison involving one deferred — a generic `BadValue` on a server with no
  Python. `type_rank` had ranked them all along.

#### Notes

Two tests asserted the limitation rather than the behaviour (`is_sortable(true)`
is false; `$maxN` over booleans errors) and were rewritten against mongod, which
handles both.

The engine-parity fuzz caught two mistakes mid-change: a lost short-circuit, and
the bool-equality rule landing in Rust before Python. One definition of BSON
equality now lives in `ordering.bson_equal` and is used by both the expression
language and the diff.

30 cases added to `tests/test_mongod_differential.py`. **The sweep is not
finished** — its remaining findings are recorded in `tasks/backlog.md`.
