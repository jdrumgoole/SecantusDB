### The Rust PostgreSQL server: a range read through an index marks its dead entries

Once a table has been analysed, the executor now reads a WHERE with the
scan EXPLAIN prints for it. Before, it priced its scan as if the table had
no statistics, with every index uncorrelated. So a range over a primary key
that EXPLAIN printed as an `Index Scan` was read as a bitmap heap scan. It
came back in heap order and marked none of the dead versions it passed.

On PostgreSQL 15.20 that index scan marks the entries of versions dead to
every snapshot (`_bt_killitems`), and later scans skip them. Here, after
`update k set b = 'y' where a between 10 and 12`, a read of `a between 8
and 14` now marks the three dead entries. The index-only scan after it
prints `Heap Fetches: 7`, as PostgreSQL does, where this server printed 10.

#### Fixed

- `secantus-pg`: `in_scan_order` and the streamed read take the path
  EXPLAIN's analysed model chooses (`executor_scan`). They fall back to the
  unanalysed estimate only for a table never analysed or a choice that
  model does not price. Where the read runs is not where the select list
  is known, so the path is priced as reading every column.

Fixes RP-1120 (`tests/backlog/test_rp_explain_costs.py::test_a_range_index_scan_marks_dead_entries`).
