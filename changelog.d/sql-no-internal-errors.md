### A function given the wrong kind of value no longer reports an internal error

`SELECT abs('abc')`, `SELECT date_trunc(1, 2)`, `SELECT repeat(ARRAY[1,2], 'x')`
and several hundred shapes like them reported `internal error`. A Python error
from inside the function was reaching the client unchanged.

They now report `function abs(unknown) does not exist`, which is what
PostgreSQL says for the large majority of them.

This came out of a deliberate hunt rather than another accident: two of these
had already turned up by chance in consecutive rounds, so every function was
tried against every value type. **397 shapes reported an internal error; none
do now.** The guard sits at the two points where functions are evaluated, not
inside each function — per-function guards are exactly what left the holes.

A function that already reports a proper error keeps it: `to_char` on an
interval with a day-name template still reports the format error, rather than
being flattened into a generic one.

#### Fixed

- A scalar function applied to a type it does not support reports
  `42883 function … does not exist` instead of `internal error`.
