### Rust MongoDB server: `explain` counts a SORT of index keys as it executes

`explain`'s SORT below a FETCH (`type: "default"`) now counts each
working-set member as mongod 8.2.11 does: its sort key, the index key it
was found at and 32 bytes. Before, it counted the documents behind the
keys, so a document's other fields moved `totalDataSizeSorted`.

#### Fixed

- `explain` in `executionStats` and in an `allPlansExecution` trial
  reports `totalDataSizeSorted` for a SORT of index keys as the profile
  entry's `execStats` and mongod do: 780 for twelve `{y, x}` int keys
  over documents carrying a 50-byte pad, where it reported 1440. The
  index scan below keeps the key it found each document at, so a
  multikey or `$or` input sizes the key the member carries.

Fixes RM-476 (tests/backlog/test_rm_found_rm375.py::test_an_explained_sort_of_index_keys_counts_its_members)

#### Found

- RM-557: an `$or` over one index sorts the fetched documents
  (`simple`) where mongod sorts the index keys below the FETCH
  (tests/backlog/test_rm_found_rm476.py::test_an_or_of_one_index_sorts_its_keys).
