### Both wire servers disable Nagle — a 200x CI stall on chatty round-trips

Neither server set `TCP_NODELAY` on accepted sockets. Reply paths write
small frames back-to-back (a reply then ReadyForQuery, one batch item's
result then the next), and with Nagle enabled the second write waits for
the peer's delayed ACK — roughly 40ms per round trip on Linux, invisible
on macOS loopback where ACKs are immediate. pgjdbc's generated-keys
batch tests, which perform 1,000 single-row round trips each, measured
41.5 seconds per test in CI against 0.2 seconds locally from exactly
this — about 20 minutes of the pgjdbc lane's in-test time on ~30 tests.
Both servers now set `TCP_NODELAY` unconditionally on every accepted
connection, as mongod and PostgreSQL do.
