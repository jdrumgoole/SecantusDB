### Rust MongoDB server: a bare `"$"` is refused as a field path

An expression operand of `"$"` alone is refused as mongod 8.2.11 refuses it,
instead of being read as a missing field.

#### Fixed

- `{$project: {f: "$"}}`, `{$toUpper: "$"}`, `{$concat: ["$", "x"]}` and
  `{$_internalFindAllValuesAtPath: "$"}` answer 16872 "'$' by itself is not
  a valid FieldPath", as mongod does; they answered `{}`, `""`, `null` and
  5511201.

Fixes RM-594 (tests/backlog/test_rm_found_rm493.py::test_a_bare_dollar_is_no_field_path)
