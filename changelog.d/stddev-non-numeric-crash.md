### `$stdDevPop` / `$stdDevSamp` no longer crash on non-numeric input

A group containing a value that wasn't a number — a string, an array, a
document — returned an internal server error. The accumulator added every value
it was handed, so Python's own arithmetic raised (`unsupported operand type(s)
for +=: 'float' and 'str'`) and the failure escaped as a generic "internal
server error". MongoDB simply ignores those values and computes the deviation
over the numbers that are there.

A quieter problem sat next to it. When a group held *no* numeric value at all,
both servers omitted the output field entirely; MongoDB always emits it, with
`null`. Code reading `doc["s"]` got a `KeyError` where a real server hands back
`None`. Booleans were also being counted as 0 and 1, which MongoDB does not do —
a group of booleans is `null`, not zero.

#### Fixed

- `$stdDevPop` / `$stdDevSamp` skip non-numeric values (string, array,
  document, boolean, null) instead of failing the aggregation, matching
  MongoDB's numeric domain of int / long / double / decimal.
- The output field is always present, holding `null` when the group contained no
  numeric value, rather than being omitted.
- Decimal input answers a double, as MongoDB does.
