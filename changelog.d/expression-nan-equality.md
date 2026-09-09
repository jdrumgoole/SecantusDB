### A NaN no longer crashes `$expr`, and it now equals itself

`find({"$expr": {"$gt": ["$v", 0]}})` over a collection holding a
`Decimal128("NaN")` answered **`internal server error`** on the Python server.
The expression language widens a `Decimal128` to a Python `Decimal` before
comparing, and `Decimal("NaN") < 0` raises `decimal.InvalidOperation` — not the
`TypeError` the comparison fallback was catching. One such document made an
ordinary query fail.

The same probe found a second, quieter one: `{$eq: [NaN, NaN]}` is **true** on
MongoDB, whose canonical order ranks the two as equal — which is also why
`find({a: NaN})` matches a stored NaN. Both servers said false. The Rust
server's `Decimal128` path had it right and its plain-`double` path did not, so
the operator's answer depended on which numeric type happened to hold the NaN.

Both were found by `tools/probes/query_result_sets.py` the first time it ran
with a **Python column** — the throwaway original had compared MongoDB against
the Rust server only, and promoting it to the shared two-server harness surfaced
a crash on the first run.

#### Fixed

- **`$gt` / `$gte` / `$lt` / `$lte` / `$cmp` against a `Decimal128("NaN")` no
  longer error.** NaN ranks below every number, so `$lt` is true and `$gt` is
  false, matching the plain-`double` NaN that already worked.
- **`{$eq: [NaN, NaN]}` is true and `$ne` false**, for all four pairings of
  `double` and `Decimal128` NaN, on both servers.
- The neighbouring rules are unchanged and now pinned: a bool is still not a
  number (`{$eq: [true, 1]}` is false), signed zeros are still equal, and
  **change detection still answers differently from equality** — `$set` of a NaN
  over the same-typed NaN reports `modifiedCount: 0`, while a `double` NaN
  replaced by a `Decimal128` NaN reports `1`.

#### Added

- `tools/probes/query_result_sets.py`, `tools/probes/update_result_documents.py`
  and `tools/probes/upsert_seeding.py` — three sweeps that had been throwaway
  scripts, now comparing both servers against MongoDB (266, 527 and 120 shapes;
  0 divergent).

Measured against mongod 8.2.11.
