### Rust PostgreSQL server: revocation lists, user name maps and a TLS reload

The Rust PostgreSQL server's TLS could not refuse a revoked client
certificate, map a certificate to a role other than the one it names, or
pick up a renewed certificate without a restart. It can now.

#### Added

- `--tls-crl-file` (`TlsConfig::crl_file`), PostgreSQL's `ssl_crl_file`. A
  client certificate a revocation list names fails the handshake. As
  OpenSSL requires of PostgreSQL, every certificate in the chain needs a
  list from its issuer that is in date. A list with no extensions, which
  `openssl ca -gencrl` writes by default, is read.
- `--tls-ident-file` and `--tls-user-map` (`TlsConfig::user_map`), a
  `pg_ident.conf` and `map=`. Under `--tls-client-cert cert` a certificate
  logs in as the roles the named map gives it, by exact name or by a
  regular expression with `\1`.
- `--tls-client-name DN` (`TlsConfig::client_name`), `clientname=DN`: the
  certificate's whole subject, `CN=alice,O=Example`, is what is compared
  with the user name or handed to the map.
- The TLS files are read again on SIGHUP, on `select pg_reload_conf()` from
  a superuser, and on `PgServer::reload_tls` / `RunningPgServer::tls_reloader`.
  Files that cannot be used leave the old ones serving.

#### Changed

- A server certificate whose signature is RSA-PSS is offered SCRAM channel
  binding, with the hash its parameters name. It was offered none.
- A server certificate signed with Ed25519 is offered channel binding too,
  and a login that takes it up fails with "could not find digest for NID
  UNDEF", as on PostgreSQL. libpq takes it up by default, so a client that
  connected to such a server before now needs `channel_binding=disable`.
- SIGHUP no longer stops a server that was started with TLS. It reloads.

Run against PostgreSQL 15.19: 480 of 480 scenarios answer the
same, and the 219 of the earlier login comparison still do.
