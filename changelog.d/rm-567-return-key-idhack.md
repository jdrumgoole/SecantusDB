### Rust MongoDB server: a `returnKey` lookup on `_id` is an IDHACK

`find({_id: 3}).returnKey()` now plans `RETURN_KEY <- IDHACK`, as mongod
8.2 does, where it planned `RETURN_KEY <- IXSCAN`.

#### Fixed

- An `_id` point lookup under `returnKey` is an IDHACK at any batch size
  and under a limit or a sort, with a SORT_KEY_GENERATOR beneath the
  RETURN_KEY for a projection's `{$meta: "sortKey"}`, and returns `{_id:
  <value>}`; a hint, `showRecordId`, a skip or a collation the `_id` index
  does not have still scans the index.

Fixes RM-567 (tests/backlog/test_rm_found_rm470.py::test_return_key_of_an_id_point_lookup_is_idhack, tests/backlog/test_rm_found_rm470.py::test_return_key_id_lookup_variants).
