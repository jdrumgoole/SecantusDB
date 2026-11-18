### A `Slow query` line names the collection a command works through

The Rust MongoDB server's slow-query line for `filemd5`, `rolesInfo` and
`convertToCapped` now names the namespace mongod 8.2.11 names, not the
command's value.

#### Fixed

- A `filemd5` line names `<db>.<root>.chunks`; a `rolesInfo` of a role,
  document or list names `admin.system.roles` (`rolesInfo: 1` still names
  `<db>.$cmd`); a `convertToCapped` names the temporary collection it built,
  `<db>.tmpXXXXX.convertToCapped.<coll>`, whose suffix is now five random
  letters or digits as mongod's.
  Fixes RM-596 (tests/backlog/test_rm_found_2026_11_13_rm495.py::test_a_line_names_the_collection_a_command_works_through)
