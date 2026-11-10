### Rust PG server: an ORDER BY key may hold a window call

An ORDER BY key holding a window call (`order by max(x::text) over (order
by x) desc`, `order by sum(x) over (order by x) desc`) is now sorted by the
call's value, as on PostgreSQL 15.19, instead of being refused with 0A000
`function max(text) is not supported yet`.

#### Fixed

- A key written exactly as a select-list target sorts as that target, as
  PostgreSQL's `findTargetlistEntrySQL99` matches it; any other key computes
  its window calls beside the select list's, including a window written only
  in the ORDER BY (`select x from t order by row_number() over (order by x
  desc)`). A positional key over an
  `inet` window target, which the network-order rewrite copies into the
  sort, is answered too.

Fixes RP-761 (tests/backlog/test_rp_found_rp754.py::test_a_window_call_in_an_order_by_key)

Filed RP-840 (EXPLAIN prints a computed sort key by its synthetic field).
