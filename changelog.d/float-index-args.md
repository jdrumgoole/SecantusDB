### Array-index operators accept a whole-number double, like mongod

`$arrayElemAt`, `$slice`, and `$indexOfArray` rejected *every* non-integer index,
but mongod accepts a **whole-number double** (`$arrayElemAt: [[...], 2.0]` →
element 2; `-1.0` → last element) and rejects only a *fractional* one. SecantusDB
now matches: a whole double is coerced to the integer index (so both servers
compute the same element), and a fractional double raises mongod's per-operator
code — `$arrayElemAt` 28691, `$slice` 28726 (second arg) / 28728 (third arg),
`$indexOfArray` 40096.

This mattered beyond fidelity: the fix had to land on both servers together —
had only the Python engine learned to accept `2.0`, the two servers would have
*disagreed* on a valid index (Python returning the element, the Rust server
erroring). Ground truth probed against mongod 7.0.12; three-way verified.

#### Fixed

- `$arrayElemAt`, `$slice`, and `$indexOfArray` accept a whole-number double
  index (coerced to int) instead of returning null/-1, and reject a fractional
  double with mongod's exact per-operator error code (both servers).
