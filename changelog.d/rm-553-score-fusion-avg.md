### `$scoreFusion` combines by `"avg"`

The Rust MongoDB server's `$scoreFusion` now takes `combination.method:
"avg"`, as mongod 8.2.11 does, and refuses `"average"` with code 2.

#### Fixed

- `$scoreFusion` accepted `"average"` and refused `"avg"`; mongod's enum is
  `avg` / `expression`, and `"average"` is only the method's name in
  `scoreDetails`.

Fixes RM-553 (tests/backlog/test_rm_found_rm358.py::test_score_fusion_combines_by_avg)
