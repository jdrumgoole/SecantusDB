### "Missing" now propagates through `$cond`, `$switch`, `$let` and `$ifNull`

mongod distinguishes a *missing* value from an explicit `null`, and the operators
whose result **is** one of their sub-expressions pass that missing-ness along. A
`$cond` whose taken branch is missing is itself missing — so `$addFields` omits
the field rather than writing a null. Both servers wrote the null.

Measured against mongod 8.2.11: **1 of 7** shapes were correct before this
(`$getField` alone).

#### Fixed

- `$cond`, `$switch`, `$let` (its `in`) and `$ifNull` now return the missing
  marker when the sub-expression they select is missing, on **both** servers.
  `{"$addFields": {"z": {"$cond": [true, "$nosuch", 1]}}}` omits `z`, as mongod
  does, instead of writing `z: null`.
- **It matters in operator position too, not just as a field value.**
  `{"$eq": [{"$cond": [true, "$nosuch", 1]}, null]}` is **false** on mongod,
  because the result is missing rather than null — we answered true. The
  comparison operators already distinguished the two for a bare field path; now
  a value reaching them *through* a control-flow operator is distinguished as
  well.
- `$ifNull` skips a missing argument exactly as it skips a null one, and returns
  missing when every argument is.

The operators that **compute** a value — `$add`, `$concat`, `$arrayElemAt`,
`$first` — still collapse missing to null, which both engines already had right.
That split is the whole rule: *return* a sub-expression and missing-ness travels,
*compute* one and it does not.

#### Notes

- `expressions.evaluate_or_missing` was a second copy of the field-value rule,
  and the copies had already drifted once: the `$$REMOVE` fix had to be written
  twice. It now delegates to the single implementation, so this change did not
  need writing twice too.
- The engine-parity fuzz suite caught the mid-port state (Python propagating,
  Rust not) on a seeded expression — exactly what it is for. Worth recording that
  the expression it failed on is one mongod itself rejects, so the parity gate is
  pinning the two engines to each other there, not to the reference server; the
  isolated question it raised was then settled against mongod directly.

25 cases added to `tests/test_mongod_differential.py`, covering both positions
and both families (propagating and computing), so the split cannot quietly move.
