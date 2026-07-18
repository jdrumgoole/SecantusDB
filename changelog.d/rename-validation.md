### $rename validates its spec instead of corrupting the document

`$rename` performed no validation, so several invalid specs silently corrupted
data or leaked a raw Python exception rather than raising mongod's error:

- `{$rename: {"arr.0": "x"}}` (source is an array element) rewrote the array to
  `[null, 2, 3]`; `{a: "arr.0"}` (destination an array element) wrote into the
  array. mongod rejects both (code 2) — the field cannot be an array element.
- `{a: "a"}` (same field) and `{a: "a.b"}` (source/target on the same path) were
  applied; mongod rejects both (code 2).
- `{a: ""}` (empty target) created a field named `""`; mongod → code 56.
- `{a: 5}` / `{a: true}` (non-string target) leaked an `AttributeError`
  (`'int' object has no attribute 'split'`); mongod → code 2.

All now raise mongod's codes on the Python server (2 for the field/path/type
cases, 56 for the empty path) and defer to `BadValue` on the Rust server — the
document is left untouched. Valid renames (including into a new nested path) are
unaffected. Three-way mongod 7.0.12-verified.

#### Fixed

- `$rename` rejects an array-element source/destination, a same-field or
  same-path rename, an empty target, and a non-string target — instead of
  silently corrupting the document or leaking a Python exception (both servers).
