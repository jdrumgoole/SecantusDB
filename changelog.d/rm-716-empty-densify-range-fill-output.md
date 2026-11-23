### Rust MongoDB server: an empty `$densify.range` or `$fill.output` is refused as mongod refuses it

`$densify.range` is now parsed as mongod 8.2 reads it, naming its own unknown field, then a missing `bounds`, then a missing `step` in document order, and an empty `$fill.output` is refused with mongod's 6050203.

#### Fixed

- `{$densify: {range: {}}}` answered "'$densify.field' is missing"; it is now 40414 "BSON field '$densify.range.bounds' is missing but a required field", as on mongod 8.2.11.
- `{$fill: {output: {}}}` answered BadValue (2), "not supported by the Rust server"; it is now 6050203 "The output field in '$fill' must contain an object with at least one element".

Fixes RM-716 (tests/backlog/test_rm_found_rm604.py::test_an_empty_densify_range_or_fill_output_is_refused_as_mongod_refuses_it)
