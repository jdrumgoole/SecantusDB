### `$stdDevPop` / `$stdDevSamp` work in expression position on the Rust server

The `$group` **accumulator** forms shipped long ago; the **expression** forms — over an array argument in `$project` / `$addFields` — never did. All 56 shapes in the probe corpus answered "operator not supported by the Rust server" where mongod computes a number, and on the standalone server that is an error, not a fallback.

They share `group::std_dev` with the accumulators, so the two forms cannot answer different numbers, and drop non-numeric members exactly as the accumulators do: mongod counts int / long / double / decimal and silently skips bool, null, string, array and document.

`tools/probes/agg_expressions.py` against the Rust server: **981 → 925**, still zero wrong values.

#### Also recorded

Analysing the remaining defers turned up that **116 of them were valid input being refused** — mongod answers and the Rust server errors. 56 were these; the other 60 are almost all a **Decimal128 operand reaching a math operator**, declined with comments reading "→ Python" on a server that has no Python. In practice a collection holding Decimal128 values cannot use most math operators there. Recorded in `tasks/backlog.md` with what a fix would need.
