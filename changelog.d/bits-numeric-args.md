### $bitsAllSet / $bitsAllClear / $bitsAnySet / $bitsAnyClear validate their argument, like mongod

The bitwise query operators mishandled several non-integer arguments. A negative
bit position (`$bitsAllSet: [-1]`) raised an *uncaught* `ValueError` (from
`1 << -1`) that surfaced without a code, a negative or fractional non-array
bitmask was silently accepted or reported the wrong code, and the Rust server
*rejected a valid whole-number-double* mask/position (`$bitsAllSet: 6.0`) because
its coercion didn't accept doubles.

All four operators now match mongod on both servers: a whole-number double is
accepted (truncated), and a fractional double, a bool, or a negative value is
rejected — a bad *bit position* with code 2, a bad non-array *mask* with code 9 —
on the Python server with mongod's messages, the Rust core deferring to
`BadValue`. Three-way mongod 7.0.12-verified.

#### Fixed

- `$bits*` accept a whole-number-double mask / bit position and reject a
  fractional / negative / bool one with mongod's exact code, instead of raising
  an uncaught `ValueError` on a negative position, silently accepting a negative
  mask (Python), or rejecting a valid `6.0` (Rust server).
