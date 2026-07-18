### $gte/$lte: null and $exists match mongod's semantics

Two query-match correctness bugs that silently returned the wrong documents (no
error): a range comparison against null and `$exists`'s argument truthiness.

`{f: {$gte: null}}` (and `$lte: null`) matched nothing; mongod matches documents
where `f` is null **or missing** — the same set as `$eq: null` — because null only
orders equal to null. Both now do. (`$gt`/`$lt: null` correctly match nothing.)

`$exists` used Python's truthiness for its argument, so `{$exists: ""}` /
`{$exists: []}` / `{$exists: {}}` were read as `$exists: false`. mongod uses its
own truthiness — only `false`, `0`, and `null` are falsy; an empty string, array,
or document is truthy — so those all mean `$exists: true`. Both servers now match
(the Rust `truthy` no longer treats empty containers as falsy, and its comparison
routes `$gte`/`$lte: null` to null-equality). Three-way mongod 7.0.12-verified.

#### Fixed

- `$gte`/`$lte: null` match null and missing (like `$eq: null`) instead of nothing.
- `$exists` uses mongod's argument truthiness (empty string/array/document are
  truthy), not Python's, so `{$exists: ""}` means exists-true (both servers).
