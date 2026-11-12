### A sort over a covered projection counts what mongod's counts

When a covered read projects before it sorts (`SORT <- PROJECTION_COVERED
<- IXSCAN`), mongod 8.2 sorts what the projection built from the index
keys: one working-set member per document, holding only that document.
The Rust MongoDB server counted each one as an index-key member, so its
SORT reported 780 bytes in `totalDataSizeSorted` where mongod reports 1584.
It now counts each member's sort key plus its document's approximate size,
in `explain` and in a profile entry's `execStats`, and spills members the
way mongod writes them.

#### Fixed

- **Sizing.** A SORT over a covered projection counts what mongod 8.2.11
  counts, for example 132 bytes for each `{x: <int>}` document sorted by
  `x`. It used to count the index keys behind them.
- **Spilling.** Past its memory budget with `allowDiskUse`, the sort spills
  the same records and bytes as mongod: 108000 bytes in 5 spills for 2000
  `{x}` documents under a 100000-byte budget. The compressed size on disk
  still differs, because mongod stamps each member with a snapshot id from
  a counter this server does not keep (RM-556, a decision pending).

Fixes RM-473 (tests/backlog/test_rm_found_2026_11_09_rm373.py::test_a_sort_over_a_covered_projection_counts_what_it_sorted).
Pinned also by
tests/mongod_live/test_projection_below_sort.py::test_a_sort_over_a_covered_projection_counts_its_documents
and `::test_a_sort_over_a_covered_projection_spills_its_documents`, and by
the `secantus-core` tests `sorter::tests::a_sort_over_a_covered_projection_*`.
