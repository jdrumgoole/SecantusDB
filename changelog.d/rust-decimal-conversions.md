### `$abs` and the `$toX` conversions accept Decimal128 on the Rust server

`$abs`, `$toBool`, `$toInt`, `$toLong` and `$toDouble` all deferred on a `Decimal128` operand — and a defer on the standalone server is an error, so a collection holding decimals could not be converted or absolute-valued at all.

**70 conversion shapes now match mongod on both servers** (the probe corpus counts 5 of them: `agg_expressions.py` Rust **907 → 902**, zero wrong values).

#### Rules that would have been wrong by assumption

Every one of these was probed against mongod 8.2.11 rather than derived:

- **`$toBool` of `NaN` is `true`** — not an error, and not false. So is `Infinity`.
- **`-0` is false**, so zero-ness ignores the sign.
- **`$abs` preserves the quantum**: `Decimal128("-2.50")` gives `2.50`, not `2.5`. No arithmetic happens, so it is implemented as a sign-strip and re-parse rather than a trip through the decimal engine.
- **Three distinct messages under code 241** — NaN, infinity, and overflow are not one failure — and the overflow message **echoes the decimal's own rendering** (`…no onError value: 1E+30`), not a normalised form.
- `$toInt` of `2147483648` overflows where `$toLong` succeeds.

#### Not in this change

`$add` / `$subtract` / `$multiply` / `$divide` / `$mod` on decimals still defer. Those go through the decimal engine where the quantum is load-bearing — `2.5 × 2` is `5.0`, and `Decimal + double` yields `4.50000000000000`, not `4.5` — and that deserves its own pass. `tasks/backlog.md` carries the probed semantics so it starts from measurement.
