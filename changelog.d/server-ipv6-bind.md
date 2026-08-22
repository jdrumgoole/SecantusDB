### The server is no longer IPv4-only

`SecantusDBServer` created its listening socket with a hardcoded
`socket.AF_INET`, so an IPv6 host was not merely unserved — it failed at bind
with a bare `gaierror` ("nodename nor servname provided") that gave no clue the
address family was the problem. Nothing in the suite bound a non-IPv4 host, so it
went unnoticed.

The family now comes from `getaddrinfo`, which also handles hostnames and gives
the correct wildcard address for an empty host. `host="::1"` serves a full
round-trip; IPv4 behaviour is unchanged.

#### Fixed

- `SecantusDBServer(host="::1", ...)` binds and serves instead of raising
  `gaierror`. Covered by `tests/test_server_bind_family.py`, which drives a real
  insert and read-back over both families.
