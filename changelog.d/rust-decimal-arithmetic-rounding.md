### Decimal128 arithmetic and rounding on the Rust server, and five crash-class bugs on the Python one

`decimal.rs` already had `add`, `mul` and `div_int` — the primitives `$sum` and `$avg` accumulate with. They were never wired into the **expression** path, so `{$add: [Decimal128("2.5"), 1]}` told the client the Rust server could not do `$add`, while `{$sum: …}` over the same values answered. The backlog had this filed as "a dependency decision"; sizing it from a probe rather than the text showed most of it needed no new numerics at all.

| | Before | After |
| --- | --- | --- |
| `agg_expressions.py`, Rust server | 32 code + 0 message | **22 + 0** |
| Wrong values, either server | 0 | 0 |

#### Fixed on the Rust server

- **`$add` / `$subtract` / `$multiply`** over a Decimal128, at 34 digits, with the quantum preserved: `2.5 * 2` is `5.0` and not `5`, and `Decimal128("2.50") + 2` is `4.50`.
- **`$ceil` / `$floor` / `$trunc` / `$round`**, including the place argument. New `decimal::round_to_exp` with the four modes.
- **`$log` validates its base before deferring a decimal argument** — the four checks run argument-type, base-type, argument-domain, base-domain, so a bad base is named even when the argument is one this engine cannot compute with.

#### Five crash-class bugs on the Python server

Each reached the client as `internal server error`, from any query:

- `{$add: [Decimal128("-Infinity"), Decimal128("Infinity")]}` — Python's decimal context **traps** `InvalidOperation`; decimal128's own answer is NaN. The context now traps nothing, which closes the class rather than the instance.
- `$trunc` / `$round` of a decimal infinity — `quantize` refuses a non-finite operand by IEEE rule.
- `$ceil` / `$floor` / `$trunc` of a **double** infinity or NaN — `math.ceil(inf)` raises `OverflowError` and `math.trunc(nan)` raises `ValueError`.

The last of those was caught by this change's own **control** assertion — a line included only to contrast the double case with the decimal one. The probe corpus contains no infinities, so none of this surface had ever been asked about.

#### Two more Python defects

- The decimal fold ran in Python's **default** context: 28 digits, so six of decimal128's 34 were silently lost. `1.000000000000000000000000000000001 + 1` answered `2.000000000000000000000000000`.
- It converted a double with `Decimal(v)`, giving `4.5` where mongod gives `4.50000000000000`. Arithmetic takes a double at **15 significant digits**, and that precision enters the quantum — a different conversion from the one the accumulators use.

#### Rules probed, not derived

- **The place sets the quantum whether or not it changed the value**: `{$round: [Decimal128("2.5"), 2]}` is `2.50`. Returning it unchanged was wrong on 40 of 210 shapes.
- **When the whole value sits below the target place**, the deciding digit is an implicit leading zero, not the coefficient's first digit — `{$round: [Decimal128("9.995"), -3]}` is `0E+3`, not `1E+3`.
- **`$ceil` / `$floor` of a decimal infinity is NaN**, while `$trunc` / `$round` pass it through and a *double* infinity passes through all four. An asymmetry, measured rather than reasoned.

#### Why the transcendentals are still deferred

Not for want of an implementation: the Python engine computes them at 60 digits. It **still disagrees with mongod in the last digit** on 5 shapes, because mongod uses Intel's decimal library — and a prior session verified at 80 digits that our value is the correctly-rounded one and mongod's is 1–2 ULP low.

The operations landed here are the ones IEEE 754-2008 defines as *correctly rounded*, which is exactly why they match exactly. Porting series to Rust would trade 16 loud "unsupported" errors for silent last-digit-wrong values, and this campaign has held wrong values at zero throughout. `tasks/backlog.md` §7 records the decision with the measurement.
