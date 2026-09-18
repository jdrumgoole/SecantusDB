### Timestamp ranges keep their microseconds, and pipelined replies wait for Sync

The PostgreSQL server stored `tsrange` and `tstzrange` bounds to the
millisecond, so a range written as `[…49.338943, …)` read back as
`[…49.338000, …)`, with no error. Plain `timestamp` columns were fixed for this
some time ago; range bounds live inside the range value and never got the fix.
They now round-trip exactly on every write path: `INSERT`, bound parameters,
`INSERT … SELECT`, `UPDATE`, `COPY` (text and binary), multiranges and arrays of
ranges.

Two older range bugs turned up along the way. Comparing a stored range with a
constructed one (`r && tsrange(…)`, `r * tsrange(…)`) failed with an internal
error, and `WHERE r = '<range literal>'` could miss a row holding exactly that
range. Both work now.

The server also sent each extended-protocol reply as soon as it had it. Real
PostgreSQL holds them until the client's `Sync` or `Flush`, so a client counting
round trips could see one pipeline turn into two. Replies are now buffered the
same way.

All three were found by re-running the driver conformance gauges with the CI
runners moved off UTC; none of them is time-zone related.

#### Fixed

- `sql/ranges.py`, `sql/typemap.py`, `sql/planner.py`: a timestamp range bound's
  sub-millisecond remainder is stored inside the range subdocument
  (`ranges.pack`) and restored on every read (`ranges._bound`, and the
  `lower_bound` / `upper_bound` accessors now used by the binary wire encoders,
  the range sort key and SQL `lower()` / `upper()`).
- `sql/ranges.py`: a naive (stored) bound orders as UTC against an aware
  (constructed) one; `stored && tsrange(…)` and `*` were `XX000`.
- `sql/planner.py`: range `=` / `<>` is evaluated per row via
  `ranges.canonical` rather than pushed down as a whole-subdocument BSON match.
- `sql/pgserver.py`: extended-protocol replies are buffered until `Sync` or
  `Flush`, or past PostgreSQL's 8 KB send buffer; an `ErrorResponse` is still
  sent at once.

#### Testing

- `tests/test_pg_range_subms.py`: every write path, containment and equality
  within one millisecond, the naive-vs-aware crash, and a comparison against a
  real PostgreSQL that runs in the `pg-oracle` CI lane.
- `tests/test_pg_pipeline_flush.py`: wire-level; nothing before `Sync`, `Flush`
  delivers without `Sync`, errors are not held back, and a large result streams.
