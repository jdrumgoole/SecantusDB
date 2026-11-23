### A missing `newRoot` is named MISSING

`$replaceRoot` / `$replaceWith` with a `newRoot` that resolves to nothing
now report mongod 8.2.11's 40228 text: "resulting value was: MISSING. Type
of resulting value: 'missing'", not null.

#### Fixed

- Fixes RM-622 (tests/backlog/test_rm_found_2026_11_13_rm520.py::test_a_missing_new_root_is_missing)
