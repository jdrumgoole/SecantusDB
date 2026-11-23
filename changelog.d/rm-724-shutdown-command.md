### `secantusd-mdb` stops on the `shutdown` command

`db.adminCommand({shutdown: 1})` now stops the server as mongod 8.2.11
does: the connection closes without a reply and the process takes the
same clean stop a SIGTERM does (maintenance threads joined, connections
drained, the final checkpoint taken).

#### Fixed

- `shutdown` answered CommandNotFound (59) and the server kept running.
  It is admin-only, as on mongod. An embedded server, whose embedder owns
  its lifetime, still leaves the command unregistered.

Fixes RM-724 (tests/backlog/test_rm_storage_model.py::test_shutdown_command_stops_the_server)
