### Rust PostgreSQL server: backlog batch 7

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- A type name keeps its case from the parse to the lookup: a domain or type
  created as `"Dom"` / `"MyType"` is reached by `"Dom"` / `"MyType"` and
  not by `dom` / `mytype`, and a type named in TEXT (`'MyType'::regtype`,
  `to_regtype(...)`) is read through the grammar as PostgreSQL's
  `parseTypeString` reads it. A record cast to an unknown type is 42704.
  Fixes RP-041
  (`tests/backlog/test_rust_postgres_gaps.py::test_mixed_case_type_is_found_only_as_written`).
- The weekly pgjdbc gauge against the Python server died at its own setup
  from 2026-10-02 on (`create database test`, which that server has no
  statement for, though it serves the name): the runner now tries the
  database first.

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
- A `pg_ident.conf` pattern is read as the SQL `~` operator reads one,
  POSIX classes and all (`[[:alpha:]]` counts `é`), so a map and a query
  agree on what a pattern matches (`ident.rs`,
  `a_pattern_reads_posix_classes_as_sql_does`).
- A revocation list scoped by an issuing distribution point is read and
  applied as OpenSSL applies it for PostgreSQL 15.19: user-only or CA-only,
  a distribution point the certificate must name (else "certificate
  unknown"), a reasons-only or indirect list never used ("unknown ca"), and
  a delta list read as a full one. Such a list was refused at startup.
  Part of RP-013, pinned by five new groups in
  `tools/probes/pg_login_tls_files.py` (`tests/test_rust_pg_login_differential.py`).
- SIGHUP never stops `secantusd-pg`: a server with no TLS or `pg_hba.conf`
  to re-read takes the signal and carries on, as a postmaster does. It
  stopped. Part of RP-013
  (`crates/secantus-pg/tests/signals.rs::sighup_does_not_stop_a_server_with_nothing_to_reload`).
- A direct-TLS connection (`sslnegotiation=direct`, PostgreSQL 17's) is
  closed as PostgreSQL 15.19 closes it, the client seeing "unexpected eof";
  it was accepted. Part of RP-013, pinned by the `direct tls` group in
  `tools/probes/pg_login_tls_files.py`.
- `pg_hba.conf`'s `+role` and `samerole` ask about membership as
  PostgreSQL's `is_member_of_role_nosuper` does: a superuser is a member of
  only the roles it was granted, where it counted as a member of all. Part
  of RP-013, pinned by `tools/probes/pg_login_hba.py` (122 of 122).
- `pg_hba.conf` takes `@file` inclusion as PostgreSQL 15 does: an
  unquoted `@name` is the tokens of the file `name` beside it, recursively.
  It was refused at startup. Part of RP-013, pinned by
  `tools/probes/pg_login_hba.py` (128 of 128) and `hba.rs`'s
  `an_at_file_holds_more_tokens`.
- A password past its `VALID UNTIL` runs the SCRAM exchange and fails it,
  as PostgreSQL 15.19 does, where without a `pg_hba.conf` it was refused
  before the exchange. Part of RP-013, pinned by
  `tools/probes/pg_login_options.py` (234 of 234).
- `pg_ident.conf` takes `@file` too: a field is the one value the file
  holds (`multiple values in ident field` for more), as PostgreSQL 15's
  `parse_ident_line` reads it. Part of RP-013, pinned by
  `tools/probes/pg_login_tls_files.py` (541 of 541) and `ident.rs`'s
  `an_at_file_is_the_one_value_it_holds`.
