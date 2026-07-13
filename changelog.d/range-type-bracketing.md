### Range operators are type-bracketed, matching mongod

MongoDB's range operators (`$gt` / `$gte` / `$lt` / `$lte`) are *type-bracketed*:
a scalar bound only ever matches values in the same BSON type bracket. SecantusDB
now honours that on both the Python and the Rust server, closing two divergences
that a three-way probe against real `mongod` surfaced.

A document-valued (or array-of-documents) field no longer errors on the Rust
server when compared against a scalar bound — `{a: {$gt: 2}}` against a
document-valued `a`, and `{items: {$elemMatch: {$gt: n}}}` over an array of
sub-documents, now cleanly no-match (as they always did on the Python server and
on `mongod`) instead of the Rust server returning a `BadValue`. And **bool is its
own bracket**: a boolean-valued field no longer spuriously matches a numeric bound
(Python's `bool` is an `int` subclass, so `True < 2` used to match on both
engines), while `bool`-vs-`bool` comparisons (`True > False`) still work. Both the
collection-scan and index-scan paths agree with `mongod` on every case.

#### Fixed

- Range operators (`$gt`/`$gte`/`$lt`/`$lte`) are now type-bracketed on both
  servers. A document/array operand against a scalar bound no-matches instead of
  erroring on the Rust server; a boolean field no longer matches a numeric bound
  (bool compares only with bool). Verified against real `mongod` 6.0 with a
  three-way probe (collection-scan and index-scan paths both).
