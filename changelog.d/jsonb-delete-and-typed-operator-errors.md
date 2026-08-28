### Two internal server errors replaced with real answers

Deleting a key from a jsonb value — `data - 'key'` — returned "internal server
error". The operator fell through to Python's subtraction, which has no meaning
for a dictionary, and the resulting failure surfaced with no indication of what
had gone wrong. It now deletes the key, along with the rest of PostgreSQL's
rules for the operator: removing a key that isn't there changes nothing,
deleting from an array works by index (counting from the end for a negative
one), a list of keys deletes each, and the combinations PostgreSQL rejects —
an integer index into an object, or deleting from a scalar — are rejected the
same way.

Separately, an arithmetic expression whose operands have no matching operator
(`'\x01'::bytea + 1`) also returned "internal server error", for the same
underlying reason: the raw Python operation was attempted and its failure
escaped. It now reports `operator does not exist: bytea + integer`, naming both
operand types the way PostgreSQL does.

#### Fixed

- `jsonb - key`, `jsonb - index` and `jsonb - key[]` work instead of returning
  an internal server error.
- An arithmetic operation with no operator for its operand types reports
  `42883 operator does not exist` instead of an internal server error.
