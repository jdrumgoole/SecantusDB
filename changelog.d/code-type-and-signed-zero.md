### `$type` called JavaScript a string, and signed zero was dropped by five operators

Six defects, found by classifying the wrong-value bucket of
`tools/probes/agg_expressions.py` instead of trusting the backlog's summary of
it. That summary said "5 wrong values, all a documented `Decimal128` precision
limitation"; the probe reported 24, so nineteen were uncharacterised.

**`bson.Code` read as a string.** `Code` subclasses `str`, so anything
dispatching on `isinstance(v, str)` classifies a JavaScript value as a string.
`expressions._type_name` was a FOURTH partial copy of mongod's type vocabulary
— the exact drift `bsontypes.py` was created to end — and it had the two bugs a
hand-rolled copy gets: `$type` of a `Code` answered `"string"`, and a compiled
pattern answered `"object"`. It is now deleted and delegated.

**A scoped `Code` is a different BSON type.** `Code("x", {})` is type 15,
`javascriptWithScope`, not type 13. The query language already drew this line;
the shared helper did not, so every error message naming a scoped `Code`'s type
was wrong. That one change closed 25 message divergences.

**Signed zero.** IEEE keeps the sign when a rounding lands on zero, and mongod
does too. `math.ceil` returns an `int`, which has no `-0.0`, so
`$ceil` / `$floor` / `$trunc` answered `0.0` where mongod answers `-0.0` — and
more than the probe showed, since its corpus held only a literal `-0.0` while
`-0.5` is an ordinary input that was equally wrong.

**The accumulator asymmetry.** `$add` / `$sum` / `$avg` fold from a ZERO
accumulator, so `+0 + -0` makes a lone `-0` come back POSITIVE; `$multiply`
folds from ONE and keeps the sign. Both engines returned `-0.0` from
`{$add: [-0.0]}`, and the Rust shortcut justified itself with a comment citing
the Python engine rather than the server. Neither `$add` bug was in the probe's
list — both were found by re-probing the neighbouring operators after the first
hit.

Against mongod 8.2.11 the probe goes from 24 wrong values and 173 message
differences to **20 and 148**. All 20 remaining are the documented
`Decimal128`-computed-in-`float` limitation, which is what the backlog now says.

#### Fixed

- `expressions.py`: `_type_name` delegates to `bsontypes.bson_type_name`;
  `$ceil` / `$floor` / `$trunc` keep IEEE signed zero; `$add` and `$avg` fold
  from a zero accumulator.
- `bsontypes.py`: a `Code` carrying a scope is `javascriptWithScope`.
- `crates/secantus-core`: the `$add` single-operand shortcut folds instead of
  returning the operand unchanged.

#### Changed

- `tests/test_rust_expressions_parity.py`: `_same` distinguishes signed zeros
  and recurses into arrays and documents, and all sixteen assertion sites go
  through it — twelve used a bare `==`, under which `-0.0 == 0.0`. The suite had
  `-0.0` in its fuzz pool and exercised `$ceil` 5,000 times a run without ever
  being able to see the difference. The remaining seven parity files still
  compare with `==`; filed in `tasks/backlog.md` §7.
