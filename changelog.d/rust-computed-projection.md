### The Rust server refused every computed projection

`find` and `findAndModify` answered `2 BadValue: projection is not supported by
the Rust server` for any projection whose value was an EXPRESSION rather than an
include/exclude flag:

```
find({}, {x: {$add: ["$a", "$b"]}})
    mongod  {_id: 1, x: 5}
    rust    2 BadValue: projection is not supported by the Rust server
```

Not just operator expressions — a bare rename `{x: "$a"}` failed too, as did a
string literal, a `null`, an array, and a `Decimal128` flag. The engine deferred
those shapes to the pure evaluator, and a defer has no Python behind it on this
server, so the refusal reached the client. The backlog had recorded this as a
deliberate "honest refusal"; it is now implemented instead.

#### The semantics, all measured against mongod 8.2.11

- **Only a number or a bool is a FLAG.** Everything else is a value:
  `{x: "plain"}` writes the string on every document, and `{x: {$literal: 0}}`
  yields `0` rather than excluding. A `Decimal128` *is* a BSON number, so
  `Decimal128("1.5")` includes and `Decimal128("0")` excludes.
- **A sub-document is classified PER LEAF.** `{n: {p: 1, z: "$b"}}` includes
  `n.p` *and* computes `n.z`, so the spec is flattened to dotted leaves before
  anything is decided. An empty sub-document at any depth is `51270`.
- **A bare reference and an expression differ on a missing field.**
  `{x: "$absent"}` omits `x` entirely; `{x: {$add: ["$absent", 1]}}` yields
  `null`. The field-value evaluator is what draws that line.
- **A computed field forces inclusion mode**, so a companion `b: 0` is the
  `31254` mix, and `_id: 0` still just drops `_id`.
- Dotted output keys build nesting through `set_path` — never a key with a
  literal dot in it, the shape `CLAUDE.md` warns about.

A projection error carrying a mongod code is now surfaced verbatim instead of
being flattened to `BadValue`; only a bare defer becomes the generic refusal.

Sweep: `42` shapes at **0 divergences** against mongod 8.2.11, comparing the raw
`find` reply including FIELD ORDER, plus `7` shapes on `findAndModify` — the
sibling command shares the engine and carried the same refusal.

#### Fixed

- `secantus-core`: computed projections implemented (`is_flag_value` /
  `is_computed_spec` / `flatten_projection_spec` / `with_computed`);
  `spec_truthy` accepts `Decimal128`.
- `secantus-commands`: `find` and `findAndModify` no longer refuse them, and a
  coded projection error keeps its code and message.
