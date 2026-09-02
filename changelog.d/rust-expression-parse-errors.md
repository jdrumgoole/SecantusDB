### Expression parse errors on the Rust server

`aggregate._expression_shape_problem` — the parse-time pass that fixed 279 shapes on the Python server — ported to the Rust command layer. Arity and spec shape are errors mongod raises while *building* the expression tree, before it folds anything, so they carry the stage's wrapper rather than the optimizer's.

| `tools/probes/agg_expressions.py` (3,968 cases) | Before | After |
| --- | --- | --- |
| Wrong error codes | 1,376 | **981** |
| Message-only differences | 4 | **6** |
| Wrong values | 0 | 0 |

#### Fixed

- **Arity** (`$indexOfArray` / `$indexOfBytes` / `$indexOfCP` / `$range` / `$slice`): 28667, with each operator's own bounds.
- **Date extractors given an array** of any length but one: 40536.
- **Object-spec expressions**: `$firstN`/`$lastN` 5787801, `$minN`/`$maxN` 5787900, `$median` 7436201, `$percentile` 7436200, `$topN`/`$bottomN` 168.
- **Unrecognised date-spec arguments** across eight operators, eight codes — several of which the Rust server had been **silently ignoring**, answering `ok` for a spec mongod refuses.

#### Left

946 of the remaining 981 are the engine deferring on a bad **argument** — "operator not supported by the Rust server" for what is a bad operand. They are spread across ~120 operators and driven by the operand type, so they are a per-operator campaign rather than another systematic pass. Recorded in `tasks/backlog.md` with the operand-type breakdown.
