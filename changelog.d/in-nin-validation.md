### $in / $nin validate their argument instead of leaking or silently no-matching

`$in` and `$nin` never checked their argument. A non-array (`{a: {$in: 5}}`)
leaked a raw Python `TypeError`, and an array element that was a document with a
`$`-prefixed key (`{$regex: …}` or `{$x: 1}`) silently matched nothing. mongod
rejects both with `BadValue`: "$in needs an array" and "cannot nest $ under $in".
Both servers now do the same.

A BSON regex *literal* (`/x/`) and a plain subdocument element remain valid. The
Python server carries mongod's `BadValue`; the Rust core defers these cases so
the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$in` / `$nin` reject a non-array argument ("needs an array") and an array
  element that is a document with a `$`-prefixed key ("cannot nest $ under $in")
  with `BadValue`, instead of leaking a Python `TypeError` or silently matching
  nothing (both servers).
