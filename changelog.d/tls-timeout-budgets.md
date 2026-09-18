### A network timeout has a direction, and the TLS tests had it backwards

Two tests failed intermittently on the Windows CI runner and nowhere else, three
weeks apart, in different suites: a pymongo client pinging a TLS-enabled Rust
server, and a raw socket completing a PostgreSQL TLS handshake. Both were
recorded as flakes and passed on rerun, which is how they survived — and the
same entry was filed in the backlog twice, once for each sighting.

Neither was a race against the listener. The server binds and listens before it
hands back an address, so a connection lands in the accept backlog whether or
not the accept thread has been scheduled; there is no readiness signal to wait
for, because the readiness signal is the handshake itself. What both tests
shared was a five-second budget that had to cover thread scheduling, a protocol
round trip and an RSA-2048 handshake on the slowest machine in the matrix while
the rest of the suite ran in parallel.

The fix is a distinction rather than a bigger number. A budget on a path
expected to succeed should be generous, because it costs nothing when the test
passes and buys only failures on slow machines; a budget on a path expected to
be refused must stay short, because the test waits out the whole of it. Those
are now two named constants, applied across every suite that negotiates TLS —
one of which had already arrived at the same split on its own.

#### Added

- `tests/net_timeouts.py`: `CONNECT_TIMEOUT_S` and
  `SERVER_SELECTION_TIMEOUT_MS` for paths expected to connect, and
  `REJECTED_SELECTION_TIMEOUT_MS` for paths expected to be refused.

#### Fixed

- The intermittent Windows failures of `test_tls_against_rust_server` and
  `test_pgserver_auth.py::test_tls_request_accepted_and_query_over_tls`.
- Budgets in `test_tls.py`, `test_x509_auth.py`, `test_pgserver_auth.py`,
  `test_pgserver_pg8000.py` and `test_rust_server_smoke.py` now come from the
  shared module instead of five sets of literals.
