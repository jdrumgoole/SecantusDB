### `$inc` on a string no longer 500s, and `$inc` on a bool no longer writes silently

A three-way differential against a real mongod turned up three defects in the
update operators.

`$inc` against a string field raised an unhandled `ValueError` that reached the
client as "internal server error"; mongod answers TypeMismatch. `$inc` against a
boolean silently computed a number, because Python treats `bool` as a subclass of
`int` — that one wrote wrong data rather than failing, which is worse. `$mul` had
both defects identically. Every non-numeric type is now refused with mongod's code
14, and the document is left untouched.

`$addToSet` compared documents with Python `==`, which ignores field order. mongod
does not: `{y: 2, x: 1}` is a different value from `{x: 1, y: 2}` and gets appended
as a separate element. Our query matcher already had this right, so `$addToSet` was
disagreeing with our own equality rule; it now delegates to the matcher so the two
cannot drift.

`$min` and `$max` are deliberately unchanged — unlike `$inc`/`$mul` they accept any
type and use BSON cross-type ordering, which was verified against the same mongod.

#### Fixed

- `$inc` / `$mul` against a string, bool, null, array or document answer
  TypeMismatch (14) instead of an internal error or a silent write.
- `$addToSet` treats field-reordered documents as distinct, matching mongod and our
  own query matcher. `tests/test_update_type_rules.py`.
