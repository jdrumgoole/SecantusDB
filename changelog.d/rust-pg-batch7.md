### Rust PostgreSQL server: backlog batch 7

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Added

- `pg_hba.conf`'s `samehost` and `samenet` match by this host's interface
  addresses and their networks (`getifaddrs`), as PostgreSQL's
  `check_same_host_or_net` does. `samehost` matched loopback only and
  `samenet` was refused at startup. Part of RP-013
  (`crates/secantus-pg/src/hba.rs`, `an_address_matches_by_its_network`).
- `inet_client_addr()`, `inet_client_port()`, `inet_server_addr()` and
  `inet_server_port()`, typed `inet` / `integer`
  (`crates/secantus-pg/tests/connection_addrs.rs`). pgwire's `ClientInfo`
  carries the connection's local address for them.
