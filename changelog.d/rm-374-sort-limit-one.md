### A sort that keeps one document counts no data sorted

A `find` or `findAndModify` whose sort keeps a single document now reports
the same sort statistics as mongod 8.2. mongod sorts a limit of one with a
sorter that keeps only its best document and counts no bytes for it, so its
SORT stage says `totalDataSizeSorted: 0` and `metrics.query.sort.totalBytesSorted`
does not move. The Rust MongoDB server counted the document it kept, in
`explain`, in a profile entry's `execStats` and in `serverStatus`.

#### Fixed

- `secantus-core` `sorter::blocking_sort`: a sort with a limit of 1 counts
  its keys but no bytes, as mongod's `LimitOneSorter` does. Limits of 2 and
  more still count the documents they keep (134 and 201 bytes for two and
  three of the six documents measured on mongod 8.2.11). Regression test:
  `sorter::tests::a_sort_of_one_counts_no_data_sorted`.
- Fixes RM-374 (tests/backlog/test_rm_found_2026_11_07.py::test_a_sort_of_one_document_counts_no_data_sorted)

#### Found

- RM-468: an aggregate's leading `$sort` and `$limit` add only the k kept
  documents to `metrics.query.sort.totalKeysSorted`, where mongod adds
  every document the sort read.
