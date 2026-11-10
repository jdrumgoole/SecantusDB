### The Rust PostgreSQL server subscripts jsonb

`j['a'][1]` now reads a `jsonb` value on the Rust PostgreSQL server, `UPDATE
... SET j['a'] = v` writes into one, and a PL/pgSQL `x['a'] := v` assigns into
a `jsonb` variable, as PostgreSQL 14 and later do. Before, the fetch and the
UPDATE were refused with a 42804 claiming jsonb does not support subscripting,
and the PL/pgSQL assignment was a 0A000.

The behaviour is a port of PostgreSQL's `jsonbsubs.c` and the `setPath` walk
in `jsonfuncs.c`: each subscript is coerced to `integer` or `text`, a missing
path is built (an object for a text step, an array for an integer one), an
array is padded with nulls up to the index, and a NULL source starts as an
empty array or object. The refusals keep PostgreSQL's SQLSTATE, message,
DETAIL, HINT and position.

#### Fixed

- `secantus-pgplan`: new `jsonb_subs` module (`jsonb_get_element`,
  `jsonb_set_element` under create / fill-gaps / consistent-position);
  a jsonb subscript's fetch, its UPDATE assignment and its result type
  (`jsonb`); the 42804 for a slice or a subscript of a type that coerces to
  neither `integer` nor `text`, with its hint and position.
- `secantus-pg`: PL/pgSQL assignment to a `jsonb` subscript.
- `secantus-pgplan`: `cannot subscript type X` now carries the container's
  position.

Fixes RP-772 (tests/backlog/test_rp_found_rp711.py::test_jsonb_subscripting)
