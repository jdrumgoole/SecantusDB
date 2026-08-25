### `$inc` / `$mul` type errors now match MongoDB exactly

Incrementing a non-numeric field is an error on every MongoDB server, and both
SecantusDB servers already refused it — but neither reported it the way MongoDB
does, in two different ways.

The Rust server answered `BadValue` (code 2) where MongoDB answers
`TypeMismatch` (14). The cause is structural: the shared operator engine signals
"I can't handle this, run the Python engine" with a single opaque value, which is
the right answer on the Python server and a dead end on the standalone Rust
server, where there is no Python to fall back to. Every such signal collapsed
into one generic code. The fix adds a small validator that names the errors we
can name — the same shape an existing `$jsonSchema` validator already uses —
leaving the opaque signal for constructs that genuinely aren't implemented.

Chasing that turned up a second problem nobody had recorded: the Python server's
*message* was wrong. MongoDB identifies the offending document by its `_id`
(`{_id: 1} has the field 'n' of non-numeric type string`); we printed the field
path instead (`{n} has the field 'n' …`). The code was right, so it had gone
unnoticed — the text simply wasn't one any real server produces.

Both servers now match MongoDB byte-for-byte on code and message, checked
three-way against a live `mongod` and the standalone Rust binary.

#### Fixed

- The Rust server answers `TypeMismatch` (14), not `BadValue` (2), for `$inc` /
  `$mul` against a non-numeric field or with a non-numeric argument.
- The Python server's type-error message identifies the document by `_id`, as
  MongoDB does, including `ObjectId('…')` rendering and the leaf field name for
  dotted paths.
