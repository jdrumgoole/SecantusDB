### Rust MongoDB server: a `$setWindowFields`, `$densify` or `$fill` field of the wrong type is a type mismatch

A stage field of the wrong type, such as `{$densify: {field: 1}}`, is now
refused as mongod 8.2 refuses it, with TypeMismatch (14) naming the field and
the type it expected, instead of BadValue "not supported by the Rust server".

#### Fixed

- The fields are typed in document order with the unknown-field check, as
  mongod's parser reads them; a null field counts as absent, and `$densify`
  without a `range` is refused for it.
- Fixes RM-604 (tests/backlog/test_rm_found_rm386.py::test_a_stage_field_of_the_wrong_type_is_a_type_mismatch)
