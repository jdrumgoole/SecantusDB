### Rust MongoDB server: `aggregate` parses `writeConcern` and `bypassDocumentValidation`

`aggregate` now checks both fields in its document-order field pass, as
mongod 8.2.11 does, whatever the pipeline writes. A mistyped one used to be
ignored, and the command answered ok.

#### Fixed

- `writeConcern` is parsed as a whole `WriteConcernOptions`. A non-object
  value is 14 "BSON field 'aggregate.writeConcern' is the wrong type ...,
  expected type 'object'". An unknown inner field is 40415, and a bad `w`,
  `j` or `fsync` gets the same error the write commands give. An explicit
  `null` is still accepted.
- `bypassDocumentValidation` takes a bool or any number, and `null`.
  Anything else is 14, with mongod's type list for this field, "[decimal,
  bool, double, long, int]".
- When two fields are wrong, the first in document order is reported, as
  on mongod.
- Fixes RM-463 (tests/backlog/test_rm_aggregate_parse.py::test_aggregate_type_checks_write_concern_and_bypass);
  regression tests in
  `tests/test_rust_arg_types_sweep.py::test_aggregate_parses_write_concern_and_bypass`.
