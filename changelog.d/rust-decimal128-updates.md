### A collection holding a `Decimal128` was un-updatable on the Rust server

`{$set: {z: 1}}` against a document containing a `Decimal128` anywhere — a
top-level field, a nested one, an array element, or the `_id` — failed with
`query uses a construct the Rust server does not support`. Nothing about the
update touched the decimal.

The oplog update-diff walks every field of the old and new document through
`py_eq` to work out what changed, and `py_eq` deferred on `Decimal128`. On the
Python server that defer is a real fallback; on the standalone Rust server there
is no Python behind it, so the whole update failed. `replace_one` and `$unset`
worked, which is what made it look like an obscure edge rather than "this
collection is read-only".

`numeric::classify` has handled `Decimal128` all along — only the fast-path
comparison declines it — so the comparison was already available and equality
simply was not asking for it.
