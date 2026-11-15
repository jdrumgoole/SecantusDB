### Rust MongoDB server: a `default` sort of documents counts its working-set members

A blocking SORT that keeps each document's metadata (`showRecordId`, a
projection reading `{$meta: "recordId"}`) is a `default` sort, and mongod
8.2.11 holds each document there as the `RID_AND_OBJ` working-set member it
was read in: 32 bytes more per document in `totalDataSizeSorted` for its
RecordId, and 19 more per spilled record. This server now counts and spills
those members, in `explain` and in execution, where it counted the bare
documents as a `simple` sort does.

#### Fixed

- `totalDataSizeSorted` and `spilledBytes` of a `showRecordId` sort match
  mongod (1122 for six `{_id, x, y, z}` documents, not 402).

Fixes RM-577 (tests/backlog/test_rm_found_rm472.py::test_a_default_sort_of_documents_counts_its_members)
