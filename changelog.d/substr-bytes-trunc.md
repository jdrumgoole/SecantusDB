### $substrBytes truncates a double index, completing substr numeric fidelity

`$substrBytes` rejected a non-integer start/length, but mongod accepts any double
there and truncates it toward zero (`1.7`→1, `2.9`→2, `0.9`→0) — unlike
`$substrCP`, which rejects a fractional double. Both servers now truncate and
compute the same substring; a truncated-negative start (`-1.7`→-1) still falls
into the negative-start rejection (50752), and a negative length still means "to
the end".

With this, `$substrBytes` / `$substrCP` numeric-argument handling matches mongod
across the board — bool rejection, byte-vs-code-point aliasing, whole-double /
fractional / truncation semantics, UTF-8-split rejection, and negative indices.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` accepts a double start/length and truncates it toward zero
  (matching mongod), instead of rejecting all non-integer values (both servers).
