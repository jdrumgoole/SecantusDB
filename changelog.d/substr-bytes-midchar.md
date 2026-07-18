### $substrBytes rejects a byte range that splits a UTF-8 character

A `$substrBytes` (or its `$substr` alias) range whose start or end falls inside a
multi-byte UTF-8 character used to return a Unicode replacement character (Python
server) or an empty string (Rust server) rather than the error mongod raises. Both
servers now reject: the Python server with mongod's exact codes — 28656 when the
starting index is a UTF-8 continuation byte, 28657 when the ending index lands in
the middle of a character — and the Rust core defers to `BadValue`.

The subtlety a fuzz run surfaced: mongod rejects a continuation-byte start *even
for an empty (length 0) range*, which the Rust core's "is the slice valid UTF-8?"
check missed (an empty slice is always valid), so both engines needed an explicit
boundary check. A negative start keeps the legacy slice semantics on both engines.
Clean character boundaries and clamped past-the-end ranges still compute. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` / `$substr` reject a byte range that splits a UTF-8 character
  (mongod's 28656 / 28657 on the Python server, `BadValue` on the Rust server),
  including an empty range that starts on a continuation byte, instead of
  returning a replacement character or empty string.
