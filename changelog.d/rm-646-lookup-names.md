### `$lookup` refuses a bad field path and a bad `let` name

The Rust MongoDB server ran a `$lookup` whose `localField` started with `$`,
whose `as` was empty (writing a field named ""), or whose `let` bound a name
that is not a user variable name. It now refuses them as mongod 8.2.11 does.

#### Fixed

- `localField: "$a"` answers 16410, `as: ""` 40352, `let: {X: 1}` 9.
  Fixes RM-646 (tests/backlog/test_rm_found_2026_11_14_rm528.py::test_lookup_refuses_a_bad_path_and_a_bad_let_name)
