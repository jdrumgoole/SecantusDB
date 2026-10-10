### The Rust PostgreSQL server offers TLS

`secantusd-pg` answered every TLS request with "not supported", so a client
set to `sslmode=require` could not connect at all. Given a certificate and its
key, it now completes the handshake.

Run against PostgreSQL 15.19 with `ssl = on` and the same certificate, psycopg
(libpq 18) gets the same answer from both servers for `sslmode=disable`,
`prefer`, `require`, `verify-ca` and `verify-full`, for a certificate signed by
another CA, for a TLS 1.2 ceiling, for a SCRAM-SHA-256 login over TLS, and for
`SHOW ssl`, `RESET ALL` and `SET ssl`.

#### Added

- `secantusd-pg --tls-cert-file PATH --tls-key-file PATH`: a PEM certificate
  chain and its private key (PKCS#8, PKCS#1 or SEC1, no passphrase). A client
  that asks for TLS gets it; one that does not is still served in the clear.
- `PgBuilder::tls(cert_file, key_file)` and `secantus_pg::bind_tls` for the
  embedded server, and `secantus_pg::TlsConfig`.
- `SHOW ssl` reads `on` when the server has a certificate.
- A TLS round trip (`sslmode=verify-full`) in the release workflow's smoke
  test, so the published binary's TLS is exercised before it ships.

#### Changed

- An option `secantusd-pg` does not know (`--tls-cert`, any other `--name`)
  is now an error. It used to be taken as the storage path, and the server
  started over a directory of that name.
- A server that cannot use its certificate does not start: a missing file, a
  key that does not match, or one flag of the pair without the other.

#### Not included

- SCRAM channel binding (`SCRAM-SHA-256-PLUS`): a client with
  `channel_binding=require` and a password is refused by its own library,
  where PostgreSQL connects.
- Client certificates (`clientcert`, `cert` authentication), `pg_stat_ssl`,
  and a way to refuse plaintext connections.
