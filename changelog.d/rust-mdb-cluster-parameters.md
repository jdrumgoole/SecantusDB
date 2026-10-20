### Rust MongoDB server: `setClusterParameter`, and `defaultMaxTimeMS` limits reads

#### Added

- `setClusterParameter` runs. It used to be 59 CommandNotFound. As on
  mongod 8.2.11, the value is stored as sent in
  `config.clusterParameters`, stamped with a `clusterParameterTime`, and
  `getClusterParameter` reads it back parsed. Before this,
  `getClusterParameter` reported every parameter at its default.
- Each of the fifteen parameters' fields is checked by mongod's rules,
  with its messages: types, bounds, the `safeInt64` and duration
  conversions, unknown and duplicate fields, and one parameter per call
  (271 cases against mongod, none different). A document written to the
  collection directly is read back, and deleting one restores the default.
- `defaultMaxTimeMS.readOperations` limits reads as on mongod. It applies
  to `find`, an `aggregate` without `$out` / `$merge`, `count` and
  `distinct` that give no `maxTimeMS`. It applies only to an authenticated
  user without `bypassDefaultMaxTimeMS`: with authorization off it does
  nothing, also as on mongod.

RM-219 part (`tests/backlog/test_rm_219_cluster_params.py::test_set_cluster_parameter_as_mongod`,
`::test_default_max_time_ms_limits_reads_without_bypass`).
