### $substrBytes rejects a bool index, and $substr is byte-based like mongod

Completing the aggregation bool-as-int sweep: `$substrBytes` computed a bool
start/length index (`as_int_like(Boolean) → 0/1`) instead of rejecting it. Both
servers now reject — the Python server with mongod's exact codes (16034 for the
starting index, 16035 for the length), the Rust core defers to `BadValue`.

While verifying against mongod, `$substr` turned out to be mis-aliased: mongod
treats `$substr` as a deprecated alias of `$substrBytes` (byte-based), but
SecantusDB routed it to `$substrCP` (code-point-based). On multi-byte strings
the two diverge, and a bool index reported the wrong code (34450 instead of
16034). `$substr` now aliases `$substrBytes` on both servers, fixing the byte
semantics and the bool code together. Three-way mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` rejects a bool start/length argument with mongod's codes
  (16034 / 16035) instead of coercing it to 0/1 (both servers).
- `$substr` is now a byte-based alias of `$substrBytes` (matching mongod),
  rather than code-point-based `$substrCP`.
