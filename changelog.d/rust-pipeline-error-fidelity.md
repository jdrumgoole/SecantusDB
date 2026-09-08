### Errors the Rust server lost, or answered as a value, inside a pipeline

Four defects, found by probing outward from the required-argument fix and all
measured against mongod 8.2.11.

**`$group` and friends discarded every named error.** `group.rs`, `fill.rs`,
`densify.rs` and `windowfields.rs` typed their errors as `Result<T, ()>`, so an
error the expression engine had already named was thrown away at the module
boundary and the client got the generic refusal instead:

```
{$group: {_id: null, x: {$first: {$ln: 0}}}}
    mongod  28766 $ln's argument must be a positive number ...
    before  2     aggregation pipeline uses a stage or operator not supported
```

Carrying `Fallback` lets a named error through; `Fallback::Defer` is the same
"cannot reproduce this" signal the unit `()` was, so all 57 existing sites keep
their exact behaviour. A parse error is reported **bare** here, which mongod does
inside `$group` / `$expr` / `$redact` and which the new `Fallback::bare()` marks.

**`$sortByCount` rejected every expression.** An unconditional early match arm
answered 40147 for any document argument, shadowing a later arm seventy lines
below that already implemented mongod's three codes correctly.
`{$sortByCount: {$add: ["$n", 1]}}` is valid and now works.

**`$arrayElemAt` answered `null` for a non-numeric index** — a silent wrong
value. Only `bool` was checked, so `{$arrayElemAt: [[1, 2], "x"]}` came back as
`null` where mongod raises 28690 naming the type. Measured across 19 index
shapes: `null` and a missing field really are null, every numeric works
(**including `Decimal128`**, which was rejected), and everything else is 28690.
"Representable as a 32-bit integer" is also enforced now, so a whole `1e40` is
28691 rather than a missing field.

**`$divide` and `$mod` by zero deferred**, each with a comment justifying it by
what "Python raises" — the shape `CLAUDE.md` catalogues, and wrong here because a
defer has no Python behind it. They now answer mongod's `2 can't $divide by zero`
and `16610 can't $mod by zero`.

#### Measured

`$group` / `$bucket` / `$sortByCount` over twelve named-error expressions plus
four valid `$sortByCount` forms: **31 of 40 matching, from 18 of 36**.
`$arrayElemAt` index types: **0 divergences of 19**. The 6,628-case expression
corpus is unchanged at 38 different-code divergences with 0 wrong values and no
regressions (it does not cover these shapes, which is why they survived so long).

#### Fixed

- `secantus-core`: `group` / `fill` / `densify` / `windowfields` carry `Fallback`;
  `Fallback::bare()` for parse errors mongod sends unwrapped; `$arrayElemAt`
  index typing and int32 range; `$divide` / `$mod` by zero named.
- `secantus-commands`: the shadowing `$sortByCount` arm removed; a bare error is
  no longer given a pipeline wrapper.
- `secantus` (the PYTHON server): the same `$arrayElemAt` index rules — it had
  the identical silent-null defect, and rejected a decimal index too.
