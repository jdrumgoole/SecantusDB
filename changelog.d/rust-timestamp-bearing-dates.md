### An ObjectId or Timestamp is a date on the Rust server too

mongod accepts every BSON type that **carries** a timestamp wherever a date is expected — a Date, an ObjectId (its 4-byte generation time) or a Timestamp (its seconds field) — and treats a one-element array as the argument itself. So `{$year: ObjectId("64b7f9a2…")}` answers `2023`.

The Rust engine deferred on all of them, and a defer on the standalone server is an **error**: 13 shapes refused input mongod answers. The Python engine took this fix in the previous change; this is the port.

`tools/probes/agg_expressions.py` against the Rust server: **925 → 912**, still zero wrong values.

#### Also scoped

The remaining valid-input refusals are **38 operators declining a `Decimal128` operand** — the whole math, comparison and conversion family — each with a comment reading "→ Python" on a server that has no Python. In practice a collection holding `Decimal128` values cannot use most math operators there.

`tasks/backlog.md` now carries the scoping rather than a guess: `decimal.rs` already represents sign / coefficient / exponent with `add`, `mul`, `div_int` and `trunc_to_i64`, so about **19 of the 38 are reachable with what exists** (`$abs` is a sign flip, `$subtract` is `add` negated, the comparisons need one `cmp`). The other ~17 are transcendental and need real decimal math. It is 19 individually-probed operators, not one change — result type, precision and overflow all differ per operator.
