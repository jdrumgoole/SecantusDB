### The structured-log regression test waits for a connection's last line

RM-302's regression test closes a second client and reads `getLog` straight away. The server writes `Connection ended` once it notices the socket closed, and that can come after the client's `close()` returns. On CI the test once found no such line. A rerun of the same commit passed. The test now waits up to five seconds for the line, but only when the server writes connection lines at all. The reference mongod runs `--quiet`, so it writes none and the test does not wait for it.

#### Fixed

- `tests/backlog/test_rm_001_028.py::test_get_log_lines_are_structured_json`: a timing race between closing a connection and reading the log.
