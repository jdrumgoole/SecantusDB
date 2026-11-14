### Rust MongoDB server: a `$near` key inside a value is an equality

A filter such as `{a: {$eq: {$near: 1}}}` or `{a: {x: {$near: 1}}}` was
refused with "geo $near must be top-level expr"; it now answers as the
equality mongod 8.2 reads it as.

#### Fixed

- The `$near` placement check looks only where mongod reads an operator
  (a field's condition, `$not`, `$elemMatch`, `$and` / `$or` / `$nor`),
  not inside the operands of `$eq`, `$ne`, `$in`, `$all` or an embedded
  document.

Fixes RM-546 (tests/backlog/test_rm_found_2026_11_10_rm421.py::test_a_near_key_inside_a_value_is_an_equality)
