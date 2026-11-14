### Rust MongoDB server: an index scan's key filter counts its own keys in the trial

A candidate plan whose IXSCAN carries a filter over the index keys (a
`$type`, say) now hands its FETCH every key that filter passes, as mongod
8.2 does, instead of only the documents the whole query matches.

#### Fixed

- The trial answered an IXSCAN's key filter with whether the whole query
  matched each document, so a losing `b_1` candidate for `{a: {$lt: 5}, b:
  {$type: "int"}}` reported 6 keys returned and 6 documents examined where
  mongod reports 101.

Fixes RM-440 (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_type_candidate_reads_mongods_keys)
