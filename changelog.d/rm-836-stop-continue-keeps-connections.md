### The server keeps its connections across a stop and continue

On Linux, `secantusd-mdb` dropped every client connection when the process was stopped and continued (`SIGSTOP` then `SIGCONT`, `docker pause` then `unpause`, a debugger attach), and now carries on as `mongod` does.

#### Fixed

- Fixes RM-836 (`tests/backlog/test_rm_836_stop_continue.py::test_connections_survive_a_stop_and_continue`, and the unit test `read_poll_tests::an_interrupted_read_is_a_poll_not_a_failure` in `secantus-server`).
  A read with a timeout (`SO_RCVTIMEO`) fails with `EINTR` after a stop and continue on Linux, whatever the signal handlers (signal(7)). The connection loops read their sockets with a 250 ms timeout and treated any error but a timeout as the client going: an idle connection, one between commands and a driver monitor's streaming `hello` were all closed at the `SIGCONT`, and every driver then cleared its pool. Measured on Linux: after a stop of half a second, `mongod` 8.2.11 answered all three connections and the released `secantusd-mdb` 0.5.3-beta.199 closed all three. `Interrupted` now counts as the poll ending, in the request reads, the streaming `hello` probe, the TLS handshake and the accept loop. The PostgreSQL server was not looked at. With the change, a `secantusd-mdb` built from this tree on Linux (aarch64) answered all three after the stop, on 2 of 2 runs.
