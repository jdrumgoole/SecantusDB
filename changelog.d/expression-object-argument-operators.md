### 25 operators that require a document argument, and the last of the sweep's crashes

The largest uniform block left in the expression sweep: **25 operators, 675
shapes** where mongod says the argument must be a document and the Rust server
answered its generic `BadValue` (2) `aggregation pipeline uses a stage or
operator not supported by the Rust server`.

#### Fixed

- **`$convert`, `$dateAdd`, `$dateDiff`, `$dateFromParts`, `$dateFromString`,
  `$dateSubtract`, `$dateToParts`, `$dateToString`, `$dateTrunc`, `$filter`,
  `$let`, `$ltrim`, `$map`, `$reduce`, `$regexFind`, `$regexFindAll`,
  `$regexMatch`, `$replaceAll`, `$replaceOne`, `$rtrim`, `$setField`,
  `$sortArray`, `$switch`, `$trim`, `$zip`** now answer mongod's own code and
  wording on both servers. It is a table because the wording is **five different
  phrasings** that are not interchangeable — `found: <type>` versus
  `found <type>` versus no type at all — and the codes range from 9 to 5439007.
- **The last of the sweep's crashes.** `{$trunc: []}` reached `arg[0]`
  (IndexError); an unrecognised key in `$cond` / `$dateToString` was a bare
  KeyError; and `$exp` / `$sinh` / `$cosh` of a large value raised
  `OverflowError` where mongod saturates to **infinity**. Each surfaced as
  `internal server error`.

  **That takes the Python server from 274 crashes to zero** across this sweep.
- `$trunc` / `$round` have a ranged arity (1–2 arguments) with mongod's own
  28667, distinct from the fixed-arity 16020.
- An unrecognised key is reported at **parse** time, so it takes mongod's
  `Invalid $<stage> :: caused by ::` wrapper rather than the executor prefix.

#### Also fixed

Code **9 is `FailedToParse`**, not `Location9`. The parse-time checks were
naming every code `Location<n>`; the codes they return are a mix, and the
differential gate caught it. Both servers now use their existing
code-name tables.

#### Where the sweep stands

| | start | now |
|---|---|---|
| Python crashes | 274 | **0** |
| Python code differences | 2288 | 682 |
| Rust code differences | 2288 | 1556 |
| Rust message-only | 689 | **0** |

32 cases added to `tests/test_mongod_differential.py`.
