### Malformed-frame edges surface as typed errors, not raw exceptions

Three input-parsing paths that could raise a raw Python exception on
attacker-controlled bytes now translate it into the subsystem's typed,
recoverable error — closing three defence-in-depth items from the
2026-07-20 security review. None was a reachable exploit (each was already
caught by an outer handler that keeps the process alive), but each leaked a
raw exception where a clean typed error belongs.

A pathologically deeply-nested `$jsonSchema` query now raises a
`FailedToParse` (code 9) instead of letting a `RecursionError` escape the
matcher to the dispatch layer's generic-internal-error handler. A truncated
or non-UTF-8 PostgreSQL startup packet now raises the wire layer's
`PGProtocolError` instead of a raw `struct.error` / `UnicodeDecodeError`,
matching the discipline the BSON framing path already had. And the Mongo
wire framing gains a defensive `RecursionError` branch that routes an
over-deep document to the same `BadValue` reply as an invalid-BSON body (at
the default recursion limit such a document is already rejected as
`InvalidBSON`; this covers the case where a `RecursionError` escapes the
decode regardless).

#### Fixed

- `$jsonSchema` with pathological schema nesting returns `FailedToParse`
  (code 9) rather than a generic internal error (security review I21).
- A malformed PostgreSQL startup packet (short `CANCEL`, non-UTF-8
  parameter) surfaces as `PGProtocolError` instead of a raw
  `struct.error` / `UnicodeDecodeError` (I16).
- The Mongo wire framing translates a `RecursionError` during body parse
  into a `BadValue` reply, keeping the connection alive (I1).
