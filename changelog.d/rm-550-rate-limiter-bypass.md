### Rust MongoDB server: the connection rate limiter has its bypass list

The Rust MongoDB server registers mongod's
`ingressConnectionEstablishmentRateLimiterBypass`, a CIDR range list
(`{ranges: []}` by default) settable at startup or at runtime, and exempts
the clients it names from the connection establishment rate limiter.

#### Fixed

- `getParameter` reports the list and `setParameter` sets it as mongod
  8.2.11 does: each range in canonical form (`127.0.0.1` reads back as
  `127.0.0.1/32`, `0:0::1` as `::1/128`), a string that is no CIDR kept as
  given, and a malformed list refused with mongod's codes (10065, 14,
  40414, 40415). A client in the list is counted in
  `queues.ingressSessionEstablishment.exemptedAdmissions` and
  `connections.establishmentRateLimit.exempted` and takes no token.
  Fixes RM-550 (tests/backlog/test_rm_355_364.py::test_the_connection_rate_limiter_has_its_bypass_list,
  tests/backlog/test_rm_355_364.py::test_the_bypass_list_reads_back_and_exempts_its_clients).
