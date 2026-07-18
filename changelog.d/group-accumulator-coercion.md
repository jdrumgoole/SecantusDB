### $group accumulators stop coercing non-numeric values

`$sum` and `$avg` accumulated whatever the input expression produced — a bool
folded in as `1`, and other non-numeric values either coerced or leaked a Python
error. mongod ignores non-numeric operands entirely: `$sum` of a group with no
numeric value is `0`, `$avg` is `null`. Both servers now match.

`$min` and `$max` compared values with Python's native `<` / `>`, which raises
on a cross-type pair (a number vs a string) — so a mixed-type field errored
where mongod returns a real extreme. They now ignore null / missing and order
every other value by BSON cross-type order (number < string < bool < …), so
`$max` over `[10, "hi", true]` is `true`, matching mongod. On the Rust server
these mixed-type groups previously deferred to a `BadValue`; they now compute.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$sum` / `$avg` ignore non-numeric operands (string / bool / null / missing)
  instead of coercing or erroring — an all-non-numeric group yields `0` / `null`
  like mongod (both servers).
- `$min` / `$max` order mixed-type values by BSON cross-type order and skip
  null / missing, instead of raising on a cross-type comparison (both servers).
