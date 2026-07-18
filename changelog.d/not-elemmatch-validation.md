### $not and $elemMatch validate their arguments

`$not` accepted any argument: `{$not: 5}` silently degraded to "not equal to 5"
instead of erroring, and an empty `{$not: {}}` was accepted. `$elemMatch` accepted
a non-object argument and mis-parsed it. mongod rejects both with `BadValue`: `$not`
"needs a regex or a document" (a non-empty one — an empty document is "cannot be
empty"), and `$elemMatch` "needs an Object". Both servers now match.

A regex or an operator document under `$not`, and an object under `$elemMatch`,
remain valid. The Python server carries mongod's `BadValue`; the Rust core defers
these cases so the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$not` rejects a scalar / array / bool / empty-document argument ("needs a regex
  or a document" / "cannot be empty"), and `$elemMatch` rejects a non-object
  argument ("needs an Object"), with `BadValue` — instead of silently degrading to
  an equality check or mis-parsing (both servers).
