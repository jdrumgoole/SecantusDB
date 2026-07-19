### $unwind validates its path, includeArrayIndex, and preserveNullAndEmptyArrays

The `$unwind` stage silently accepted a malformed spec: a non-`$`-prefixed `path`,
a non-string `path`, a non-string / empty / `$`-prefixed `includeArrayIndex`, and a
non-bool `preserveNullAndEmptyArrays` (which it coerced with Python's `bool()`).
mongod rejects each: a non-string path is `Location28808`, a bare path is
`Location28818`, a non-string / empty `includeArrayIndex` is `Location28810`, a
`$`-prefixed one is `Location28822`, and a non-bool `preserveNullAndEmptyArrays` is
`Location28809`. Both servers now match.

The Python server carries mongod's codes (including its verbatim double-space
message quirk for `28810`); the Rust core defers the invalid cases so the Rust
server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$unwind` rejects a non-string / bare `path` (`28808` / `28818`), a non-string /
  empty / `$`-prefixed `includeArrayIndex` (`28810` / `28822`), and a non-bool
  `preserveNullAndEmptyArrays` (`28809`), instead of silently accepting or coercing
  them (both servers).
