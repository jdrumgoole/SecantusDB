### A numeric path component over an array matched the wrong documents

mongod reads `{"v.0": …}` **both** ways and matches on either:

- the **element at that index** — and having spent the path step on the index,
  it does *not* re-apply the implicit array traversal, so `{"v.0": 1}` does
  **not** match `{v: [[1, 2]]}` even though `1` is inside `v.0`;
- the **field of that name** in each element, so `{"v.0": 9}` *does* match
  `{v: [{"0": 9}]}`.

Both servers had both halves wrong, in opposite directions — they applied
membership after the index and never tried the field reading — so a query could
both return documents mongod would not **and** miss ones it would, silently and
with no error.

Measured against mongod 8.2.11 over 22 filters × a 14-document corpus:
**11 of 22 diverged**, now 0.

#### Fixed

- The matcher's path resolver now produces both readings, and marks a value
  reached positionally so the implicit one-level traversal skips it. In Python
  that is a `list` subclass, so the value is still an array for `$size`,
  `$type`, `$elemMatch` and whole-array equality; in Rust it is a `Cand`
  carrying the flag beside the borrowed value.
- The Rust **raw-BSON fast lane** carries the same provenance. Without it the
  two lanes would answer the same query differently depending on which ran.

Combining the two readings with a plain OR would have been wrong: `$ne` and
`$nin` mean *no candidate matches*, over the combined set, not *either reading's
negation holds*. The flag travels with each candidate for that reason.
