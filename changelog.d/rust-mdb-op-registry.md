### Rust MongoDB server: a registry of running operations for `currentOp` and `killOp`

#### Added

- Every command is registered under an opid while it runs. `currentOp`
  lists them all, including another client's waiting `getMore` (`op:
  "getmore"`, its `ns`, `command` and `cursor`). `$currentOp` lists them
  too, and with `idleCursors: true` it adds every cursor no running getMore
  is reading (`type: "idleCursor"`). Before, both answered one synthetic row
  for the request itself.

#### Fixed

- `killOp` marks the operation its opid names. A waiting tailable getMore
  then ends with 11601 "operation was interrupted" under the getMore
  executor's wrapper, as on mongod 8.2.11. Before, `killOp` treated the
  opid as a connection id and closed that connection, so the opid of the
  caller's own `currentOp` row closed the caller's connection.

RM-216 part
(`tests/backlog/test_rm_found_2026_10_20b.py::test_current_op_lists_idle_cursors`,
`tests/backlog/test_rm_found_2026_10_20b.py::test_current_op_sees_a_running_operation`).
Still open: `killOp` does not interrupt a long scan, and the rows lack some
of mongod's fields.
