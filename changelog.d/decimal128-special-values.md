### `Decimal128` NaN and infinity now answer in the math operators

`$sqrt`, `$exp`, `$ln`, `$log10`, `$degreesToRadians` and `$radiansToDegrees`
refused a `Decimal128` NaN or ±Infinity on the **Rust server** — `2 BadValue:
aggregation pipeline uses a stage or operator not supported by the Rust server`
— where mongod answers. The **Python server** answered, but wrongly in five
places.

These values carry no precision, so they need none of the 34-digit decimal math
a *finite* decimal would; that is what makes them separable from the rest of the
family, which still defers on the Rust server.

Four of the rules defeat a guess, and all were measured against 8.2.11:

- `$ceil` / `$floor` of a Decimal **infinity** are `NaN`, not the infinity;
- `$ln` / `$log10` of a Decimal **NaN** come back as a **double** `nan` — the
  one place in this family where the argument's type is not kept;
- `$cosh(-Infinity)` is `+Infinity`;
- `$abs(-0)` is `0` while `$trunc(-0)` is `-0`.

The Python engine's bug was one shape repeated: its domain guards tested
`isinstance(v, (int, float))`, so a `Decimal128` slipped past them into the
decimal path and came back `NaN` — `$sqrt(Decimal128("-Infinity"))` returned
`NaN` where mongod raises `28714`, and `$ln` / `$log10` of a Decimal
`-Infinity` returned `NaN` where mongod raises `28766` / `28761`.

**The parity suite was green throughout.** Neither engine's corpus contained
these shapes, so the two engines were pinned to each other while one deferred
and the other answered wrongly. Only comparing against a real mongod separated
them — the "parity is not correctness" case, again.

#### Fixed

- `secantus.expressions`: `$sqrt` / `$ln` / `$log10` apply their domain checks
  to a `Decimal128` as mongod does, instead of letting it reach the decimal
  path and return `NaN`.
- `secantus-core`: the same six operators answer a special `Decimal128` rather
  than deferring, keeping the argument's type.
