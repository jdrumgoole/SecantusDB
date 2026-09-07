### The Rust server answered a value where mongod rejects the expression

`{$regexMatch: {}}` returned **`false`**. `{$filter: {}}` returned `null`. So did
`{$trim: {}}`, `{$reduce: {}}`, `{$map: {}}` and `{$dateAdd: {}}` — twenty-five
cases in all where an operator was missing a REQUIRED argument and the server
answered a value a caller can branch on, from an expression mongod refuses to
run at all.

The cause is the missing-vs-null conflation `CLAUDE.md` catalogues: the
operators read their required fields with the evaluator's optional-field helper,
which reports an absent key as null. `{$trim: {input: null}}` really is legal and
really does yield null — only an ABSENT key is an error — so the fix tests key
presence, never the value.

A further thirty-two cases answered the generic
`2 BadValue: aggregation pipeline uses a stage or operator not supported by the
Rust server`, which blamed the operator when the argument was at fault.

#### The rules, all measured against mongod 8.2.11

- Every `(operator, missing field)` pair has its **own** code and wording, and
  they are not derivable from a pattern: `$filter` says
  `Missing 'input' parameter to $filter` (28648) where `$reduce` says
  `$reduce requires 'input' to be specified` (40077), and `$replaceAll`'s three
  fields descend 51749 / 51748 / 51747 as you read them left to right. Hence a
  table of 40 measured pairs.
- **An UNKNOWN argument outranks a missing one.** `{$trim: {k: 1}}` is
  `50694 $trim found an unknown argument: k`, and so is
  `{$trim: {input: "a", k: 1}}`. The operators already emit those correctly, so
  the new check stands aside whenever a key is unrecognised. The corpus caught
  this: a first version got the precedence backwards and traded twenty fixed
  cases for twenty broken ones at an unchanged total.
- **The stage decides the wrapper.** A parse error from `$addFields` /
  `$project` / `$set` is wrapped `Invalid <stage> :: caused by :: …`; the same
  error inside `$match`'s `$expr` is BARE. So validation runs per stage, where
  the stage name is known, mirroring mongod's own parse-time check.
- `$switch` with `branches: []` and `$zip` with `inputs: []` get the same code
  as omitting the field entirely.

#### Measured

57 required-field cases at **0 divergences** (from 57, twenty-five of them
silently wrong), and the 6,628-case expression corpus improves from 58
different-code divergences to **38**, with 0 wrong values and no regressions.

#### Fixed

- `secantus-core`: `check_required_fields` + `validate_expression_args`, gated
  on `all_fields_recognised` so unknown-argument errors keep precedence. The
  field-value dispatch path (`$cond` / `$switch` / `$let` / `$ifNull`, which
  bypasses `apply_op`) validates too.
- `secantus-commands`: `validate_stage_expr_args` applies mongod's per-stage
  wrapper at parse time.

#### Known gap

`$group` still answers the generic refusal for these and for errors that were
already named elsewhere (`$ln: 0`'s 28766): `group.rs` types its module as
`Result<T, ()>` and discards the error. Pre-existing and independent; filed in
`tasks/backlog.md`.
