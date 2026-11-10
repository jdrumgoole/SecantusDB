### A slot-based sort counts only the bytes it spills while reading

When mongod 8.2 pushes a `$group` down into its slot-based engine, the
`$sort` in front of it adds its keys to `metrics.query.sort.totalKeysSorted`.
It adds bytes to `totalBytesSorted` and the profile's
`sortTotalDataSizeBytes` only for the runs it spills while it is still taking
input. The Rust MongoDB server used to count every row it sorted, so a sort
that never spilled reported 17700 bytes where mongod reports none. It now
counts what mongod counts: nothing for a sort held in memory, nothing for its
last run, and the rows held at each earlier spill.

#### Fixed

- Rust MongoDB server: an SBE sort (`[{$sort: ...}, {$group: ...}]`) adds no
  bytes to `totalBytesSorted` / `sortTotalDataSizeBytes` unless it spills
  while reading its input. It then adds the bytes it held at each of those
  spills, as mongod 8.2.11 does (274512 for 300 rows of 1064 bytes under a
  50000-byte budget). An SBE sort with a `$limit` adds no bytes.
  Fixes RM-398 (tests/backlog/test_rm_326_sort_counts.py::test_sbe_sort_bytes),
  with the crate test
  `sorter::tests::an_sbe_sort_counts_only_the_bytes_it_spills_while_reading`.
- Filed while working: RM-504 (an SBE sort row keeps a field only its key
  reads), RM-505 (a limited SBE sort counts only the keys it keeps), RM-506
  (an SBE sort's spilled records are a byte longer than mongod's).
