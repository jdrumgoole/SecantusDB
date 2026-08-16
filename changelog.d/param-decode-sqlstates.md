### Malformed parameters get proper SQLSTATEs, never internal errors

Two parameter-decode crashes surfaced by the pgtest byte-exact corpus:
a binary array parameter with a structurally-bogus header (bad element
oid, missing element data) and an empty-string text parameter cast to
an array type (`''::JSON[]`) both escaped as internal `XX000` errors. A
malformed parameter is client input, and PG classifies it precisely:
truncated binary data is `08P01` (insufficient data left in message),
and a bad array literal is `22P02` (malformed array literal). Both
paths now raise the right SQLSTATE through the normal error machinery.

#### Fixed

- `sql/pgextended.py`: structurally-invalid binary array parameters
  raise `08P01` instead of an internal error.
- `sql/scalar.py`: a malformed array-literal cast raises `22P02`
  instead of an internal error.
- `sql/pgwire.py`: the test-side `build_bind` helper accepts binary
  parameter format codes.
