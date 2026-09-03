### A partial index hid every NaN row, and `$min`/`$max` ranked NaN by IEEE rules

Widening two probe corpora to include values no earlier sweep had ever
contained — NaN, the infinities, `Decimal128`, `MinKey`/`MaxKey`, `-0.0` — found
two defects, present identically on both servers.

**A partial index silently dropped rows.** A partial index on
`{b: {$lte: 1.5}}` made `find({b: NaN})` return nothing; the same query on the
same documents returned the row with no index, and MongoDB returns it in both
cases. The implication check decides whether a user's query is narrow enough to
be answered from a partial index, and it compared encoded sort keys — where NaN
orders below every number — concluding that a NaN equality was covered by a
`$lte: 1.5` filter that in fact excludes it. NaN is the value that separates the
two orderings MongoDB maintains: sort places it below `-Infinity`, but every
range operator excludes it while equality matches it. The type-bracket gate
added for the previous instance of this bug class cannot catch it, because NaN
is *inside* the numeric bracket.

**`$min`/`$max` used the wrong comparison.** Both compared with IEEE semantics,
under which every NaN comparison is false, so `{$min: {a: NaN}}` over `a: 5`
left the field at `5`. MongoDB ranks the operands by sort order, where NaN is
below `-Infinity`, and writes the NaN. Both engines now compare through the
sort-key encoder they already had — the operators were simply bypassing it.

`index_result_sets.py` is 0 of 1,803 on both servers after the fix (it found the
first defect at 1 of 1,788), and `update_operators.py` closes the Python column
at 0 of 70 shapes.

#### Fixed

- `storage.py`, `crates/secantus-storage`: the partial-index implication check
  refuses any non-equality bound involving a NaN on either side.
- `update.py`, `crates/secantus-core`: `$min`/`$max` rank operands by sort
  order, not IEEE comparison.

#### Changed

- `tools/probes/index_result_sets.py`: 11 more values in the corpus (NaN, the
  infinities, `Decimal128`, `MinKey`/`MaxKey`, `ObjectId`, a date, an `Int64`),
  and `PROBE_SEED` / `PROBE_TRIALS` overrides so a sweep can be re-run at a
  different seed rather than only deeper.
- `tools/probes/update_operators.py`: 34 more shapes, 36 → 70, covering the
  same value classes across `$inc`/`$mul`/`$min`/`$max`/`$set` — including the
  NaN-versus-infinity `$min`/`$max` pairs, which a test had asserted against our
  own servers only.
