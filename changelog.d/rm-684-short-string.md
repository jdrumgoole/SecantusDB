### Rust MongoDB server: a 12-byte string is counted inline, as mongod does

mongod 8.2.11's classic `Value` keeps a string of up to 12 bytes inline; this
server kept only 11, so every memory figure and budget counted 36 bytes more
for each 12-byte string (a `$push` of one, a `$sort` of a document holding one).

#### Fixed

- `memlimit::SHORT_STR` is 12, matching mongod's classic short-string storage.
  Fixes RM-684 (tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_twelve_byte_string_is_kept_in_its_value)
