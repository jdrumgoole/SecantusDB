### Projection $slice validates its argument

A projection `$slice` silently accepted a malformed argument and returned the
full or a wrong array. mongod validates it: the valid forms are a number
(first / last n) or `[skip, limit]` with a **positive** limit; anything else is
evaluated as the aggregation `$slice` *expression* and errors. A non-number
scalar or an array with fewer than two / more than three elements is
`Location28667` (wrong argument count); a two- or three-element array whose first
element isn't a number is `Location28724`. A negative `[skip, limit]` limit and a
three-element array are rejected the same way. Both servers now match.

Valid forms — a number, and `[skip, limit]` with a positive limit (the skip may be
negative) — are unaffected. The Python server carries mongod's codes; the Rust
core defers the invalid shapes so the Rust server rejects them too. Three-way
mongod 7.0.12-verified.

#### Fixed

- A projection `$slice` rejects a non-number scalar / short / long array
  (`Location28667`) and a two/three-element array that isn't `[skip, positive
  limit]` (`Location28724`), instead of silently returning the full or a wrong
  array (both servers).
