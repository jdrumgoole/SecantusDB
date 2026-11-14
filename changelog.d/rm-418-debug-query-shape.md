### Rust MongoDB server: `$querySettings` gives `debugQueryShape`

`{$querySettings: {showDebugQueryShape: true}}` now adds each setting's
`debugQueryShape` (its query's shape with each literal a debug type
string, `{x: {$eq: "?number"}}`), as mongod 8.2 does. Before, the option
was parsed and ignored.

#### Fixed

- `showDebugQueryShape` gave no `debugQueryShape`.
  Fixes RM-418 in part (tests/backlog/test_rm_219_diag_query_settings.py::test_show_debug_query_shape)
