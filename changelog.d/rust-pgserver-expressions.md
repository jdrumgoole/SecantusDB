### Arithmetic and string expressions in the Rust PostgreSQL server

`SELECT 1+1`, `SELECT 'a'||'b'`, `SELECT 7/2` and their relatives now work.
Expressions in a select list were the single largest thing standing between the
Rust PostgreSQL server and psycopg's test suite, and the score moved from 746 to
853 of 4,238 — the largest jump so far.

The corners were measured against a real PostgreSQL rather than assumed, and
two of them are easy to get wrong. Integer division truncates, so `7/2` is 3
rather than 3.5, and dividing by zero is an error rather than a null or an
infinity. Adding anything to NULL gives NULL, and concatenating a number to a
string converts the number.

Arithmetic on decimals is deliberately still refused. PostgreSQL treats `1 +
1.5` as its `numeric` type with particular scale rules, not as a floating-point
number; returning a double would give the right value under the wrong type, and
that is exactly the class of bug that recently made a correctly-converted
integer arrive at the client as a string. Explicit floating-point casts work,
because then the type genuinely is floating point.

#### Added

- Arithmetic (`+ - * / %`), string concatenation (`||`), comparison operators
  and unary minus in a `SELECT` list, including over bound parameters.
- PostgreSQL's `division by zero` and `integer out of range` errors.
