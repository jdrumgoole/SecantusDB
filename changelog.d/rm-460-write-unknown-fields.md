### Rust MongoDB server: `insert`, `update` and `delete` refuse fields mongod does not know

`originalQuery` and `originalCollation` on any of the three, and a
command-level `sampleId` on `update`, were taken and ignored, and the write
ran. mongod 8.2.11 answers 40415 (`BSON field 'insert.originalQuery' is an
unknown field.`) and writes nothing; so does this server now.

#### Fixed

- Fixes RM-460 (tests/backlog/test_rm_found_2026_11_09_rm087.py::test_a_write_refuses_fields_mongod_does_not_know)
