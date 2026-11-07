### A write's slow-query line counts the locks and flow control mongod's does

The `Slow query` line and profile entry of a write on the Rust MongoDB
server now carry the locks mongod 8.2 takes on a replica set's primary, and
its flow control tickets. A retryable insert under the default
`w: "majority"` reads `{ReplicationStateTransition: {w: 4}, Global: {r: 1,
w: 2}, Database: {w: 2}, Collection: {w: 2}}` with `flowControl:
{acquireCount: 1}`, as mongod's does: the write's own round, the session it
checks out, its session's row in `config.transactions` (now written under
the write's own locks), and the wait for the journal.

These are real acquisitions on the server's lock manager, so
`serverStatus.locks` counts them too. Writes that write nothing, and the
per-statement locks of `update` and `delete`, still differ from mongod's and
stay on the backlog.

#### Fixed

- A write's line and profile entry carry `flowControl: {acquireCount: n}`:
  a ticket for each global IX lock an operation takes while not holding
  the global lock already, outside a transaction (mongod's
  `getFlowControlTicket`); none is ever waited for on a single member.
- A retryable write takes the replication state transition lock once for
  its session, and writes its session's row inside the write's locks with
  a nested round on `config.transactions`, which takes no ticket.
- The journal wait of a `j: true` or `w: "majority"` write takes the
  transition lock in IX and the global lock in IS, as mongod's does.
- `findAndModify` takes its full round inside the first, holding both:
  one flow control ticket, as on mongod.
- `usersInfo` and `rolesInfo` take no default write concern (they refuse
  one, 72) and so no longer wait for the journal.
- Fixes RM-357 in part (tests/backlog/test_rm_355_364.py::test_a_write_reports_mongods_locks_and_flow_control);
  the rest stays open on
  tests/backlog/test_rm_355_364.py::test_more_writes_report_mongods_locks.
