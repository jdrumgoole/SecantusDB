### The expression engine's MISSING is a case of its own, as mongod's EOO

RM-635 told a stored `undefined` apart from MISSING by carrying the real one as a
one-key document with a NUL-led key, which an operator that did not turn it back
would have read as an object. mongod gives MISSING a case apart from every BSON
value (an EOO `Value` in the classic engine, `Nothing` in SBE), so this server's
expression evaluator now does: `expressions::Field` is `Missing` or `Value`, the
operators that can answer MISSING (`$arrayElemAt`, `$first`, `$last`,
`$getField`, `$meta`) return one, and `$cond`, `$switch`, `$let` and `$ifNull`
pass either through. A `$let`, `$map`, `$filter` or `$reduce` variable binds a
value or MISSING (`Ctx::missing`). `bsondoc::held_undefined` is gone.

#### Fixed

- `{$add: ["$a", 1]}` and `{$multiply: ["$a", 2]}` over a stored `undefined` are
  null, as mongod's `Value::nullish()` has it; this server refused them with code
  14.
- A `$lookup` `let` variable bound from a stored `undefined` reads `"undefined"`
  in `$type`, as mongod's does, where it read `"missing"`.

Fixes RM-761 (tests/backlog/test_rm_found_2026_11_22_rm635.py::test_arithmetic_over_a_stored_undefined_is_null)
Fixes RM-762 (tests/backlog/test_rm_found_2026_11_22_rm635.py::test_a_lookup_let_variable_keeps_a_stored_undefined)
