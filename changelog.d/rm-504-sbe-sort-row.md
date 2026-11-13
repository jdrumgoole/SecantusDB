### An SBE sort's row keeps only what the stages after it read

The Rust MongoDB server's slot-based sort (a `$sort` with a `$group` or
`$lookup` pushed down behind it) now holds, beside its key, only the fields
the stages after the sort read, as mongod 8.2.11 does. It used to keep every
field the whole pipeline read, so a field only the sort key read (`n` in
`{$sort: {n: 1}}`), one only a `$match` in front of the sort read, and the
whole parent of a dotted key were counted and spilled too.

#### Fixed

- `totalBytesSorted`, `sortTotalDataSizeBytes` and the number of spills of
  such a sort now match mongod: 274512 bytes over 13 spills for 300
  documents of a 1000-byte string under a 50000-byte budget, where this
  server counted 276834 (and 320754 or 321048 over 14 spills for a compound
  or dotted key).

Fixes RM-504 (tests/backlog/test_rm_326_sort_counts.py::test_sbe_sort_row_leaves_out_a_field_only_the_key_reads, tests/backlog/test_rm_326_sort_counts.py::test_sbe_sort_row_keeps_what_the_stages_after_it_read)
