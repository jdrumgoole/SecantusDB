### Rust MongoDB server: a multikey index covers a field that holds no array

An index on which only some fields hold arrays now answers a projection of
its other fields from the keys, as mongod 8.2.11 does: `find({a: {$lt: 5}},
{a: 1, _id: 0})` over `a_1_c_1`, with arrays only in `c`, plans a
PROJECTION_COVERED over the IXSCAN instead of fetching each document, which
also changes the trial and the cached plan.

#### Fixed

- Covering checks the multikey paths of each projected field, not whether
  the index is multikey anywhere.

Fixes RM-441 (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_multikey_index_covers_a_field_that_holds_no_array)
