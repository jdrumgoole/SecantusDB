### Fixed-arity expression operators answer mongod's 16020 — and a one-element list is one argument

The largest block left by the expression sweep: **~907 shapes** where mongod
answers `16020 Expression $x takes exactly N arguments. M were passed in.` and we
answered a mix of 14 / 28765 / 51044 / 51276 — **233 of them by crashing** with
`internal server error`, an operator indexing `arg[0], arg[1]` on a scalar.

#### Fixed

- **65 fixed-arity operators now check their argument count**, with mongod's own
  code and wording, on both servers. It is a PARSE error: an empty or missing
  collection reports it, as mongod does.
- **A one-element list is ONE argument** — and getting this wrong was producing
  silent **wrong values**, not merely wrong errors:

  | expression | mongod | before |
  |---|---|---|
  | `{$size: [[1, 2]]}` | `2` | `1` (counted the outer list) |
  | `{$size: ["$arr"]}` | `2` | `1` |
  | `{$toUpper: ["a"]}` | `"A"` | `["a"]` |
  | `{$type: [5]}` | `"int"` | `"array"` |
  | `{$first: ["$arr"]}` | `1` | the whole array |
  | `{$abs: [5]}` | `5` | an error |

  This was found by a test written for the arity fix, not by the sweep — the
  sweep's corpus never paired an operator with a single-element list.

#### How the table was built

**By asking mongod**, not from documentation: each of the 143 operators was
called with 0-4 arguments and the arity read out of its own error message. That
also surfaced the three rules a table alone would have missed — `$cond`'s object
form (`{if, then, else}`) carries all three arguments and is exempt; `$substr` is
reported under its canonical name `$substrBytes`; and the count is `len` for a
list but 1 for anything else, including a nested expression document.

The table lives in the engine rather than the command layer, because the
evaluator needs it for the unwrap as well as the command layer for the error.

#### Result

| | crashes | wrong 16020 | message-only |
|---|---|---|---|
| Python before / after | 274 → **21** | ~907 → **0** | 689 → 669 |
| Rust before / after | 274 → **1** | ~907 → **0** | 689 → **0** |

The remaining Python crashes are the Decimal128 family, already recorded. The
remaining Rust code differences are the engine's per-operator operand-type
errors — a separate family, also recorded.

21 cases added to `tests/test_mongod_differential.py`, including the exempt
forms and the parse-time (missing-collection) case.
