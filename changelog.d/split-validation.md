### $split validates its arguments instead of leaking a Python error

`$split` with an empty separator leaked a raw Python `ValueError` (`empty
separator`), and its type / arity errors surfaced with a generic code. mongod
rejects each with a specific Location code: an empty separator is `40087`, a
non-string first / second argument is `40085` / `40086`, and the wrong number of
arguments is `16020`; a null string or separator still yields `null`. The Python
server now carries these codes; the Rust core already defers every invalid case,
so the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$split` reports mongod's Location codes — empty separator `40087`, non-string
  first / second argument `40085` / `40086`, wrong arity `16020` — instead of
  leaking a Python `ValueError` or using a generic code (both servers).
