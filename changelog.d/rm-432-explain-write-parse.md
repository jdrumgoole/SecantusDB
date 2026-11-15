### Rust MongoDB server: an explained write is parsed in full first

`explain` of an `insert`, `update` or `delete` now parses the write as
mongod does before explaining it: its fields first (a missing `documents`,
`updates` or `deletes` is 40414, a wrong type 14, an unknown field 40415),
then the namespace (73), then the batch's size (16). The server answered
"Cannot explain cmd: insert" (20) or refused the namespace before any of
these. Measured against mongod 8.2.11.

#### Fixed

- Fixes RM-432 (tests/backlog/test_rm_431_432.py::test_explain_parses_the_write_it_explains)
