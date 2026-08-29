### `$limit` and `$skip` argument errors match MongoDB exactly

A malformed `$limit` or `$skip` reported the right error code with the wrong
text, and in a few cases the wrong outcome entirely. MongoDB echoes the value it
rejected in shell form — `"x"`, `true`, `[ 1, "a" ]`, `{ a: 1 }` — where the
Python server printed a Python representation, and the Rust server printed a
Rust one.

Probing the two servers against a real MongoDB across the whole value space
turned up more than the wording. `$skip: 1.5` and `$skip: -1` came back as a
generic error from the Rust server rather than the specific one; so did
`$limit: 0`. A decimal argument was rejected by both servers where MongoDB
accepts it — `$skip: Decimal128("2")` is a valid skip of one document — and a
fractional decimal has its own message, distinct from a fractional double's.

All 44 shapes now agree across MongoDB, the Python server and the Rust server,
including ObjectId and date arguments.
