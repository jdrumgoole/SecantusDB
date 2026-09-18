"""Network budgets for tests that talk to a freshly-started server.

Two kinds of timeout live here because they must move in OPPOSITE directions,
and conflating them is what produced a recurring CI flake.

**A budget on a path that is expected to SUCCEED must be generous.** It costs
nothing when the test passes -- the call returns as soon as the server answers
-- and the only thing a tight value buys is a failure on a slow machine. The
5-second budget these suites used had to cover, on the slowest runner in the
matrix and under a fully parallel suite: the accept thread being scheduled, a
handler thread spawned, an ``SSLRequest`` round trip, and an RSA-2048 TLS
handshake. It did not always fit. `test_tls_against_rust_server`
(`storage-engine (windows-latest)`, first seen 2026-08-29 on PR #1089) and
`test_pgserver_auth.py::test_tls_request_accepted_and_query_over_tls`
(2026-09-17 on PR #1461, `_ssl.c:990: The handshake operation timed out`) are
the same bug in two suites: the budget, not the server.

There is no bind race behind them -- ``SecantusPGServer.start()`` calls
``bind()`` and ``listen()`` before returning, so a connect lands in the backlog
whether or not the accept thread has been scheduled. Waiting for readiness is
therefore not available as a fix: the readiness signal *is* the handshake.

**A budget on a path that is expected to FAIL must stay short**, because the
test pays the whole of it. A rejected TLS client is refused as fast as the
server can refuse it; a generous budget there would only add dead wall-clock to
every run. So `REJECTED_SELECTION_TIMEOUT_MS` is deliberately small and must
not be "fixed" to match the others.

One value per direction, not a per-platform table: a budget that is never
reached is free on a fast machine too, so there is nothing to gain from making
Windows special and a drift hazard in maintaining it.
"""

from __future__ import annotations

#: Seconds for a socket operation on a path expected to succeed -- connecting
#: to a just-started server and completing a TLS handshake against it.
CONNECT_TIMEOUT_S = 30.0

#: pymongo's ``serverSelectionTimeoutMS`` for a client expected to connect.
#: Server selection spans the TLS handshake, so it needs the same headroom.
SERVER_SELECTION_TIMEOUT_MS = 30_000

#: pymongo's ``serverSelectionTimeoutMS`` for a client expected to be REFUSED
#: (no client certificate, wrong CA, ...). Short on purpose -- see the module
#: docstring. Raising this makes the suite slower and fixes nothing.
REJECTED_SELECTION_TIMEOUT_MS = 2_000
