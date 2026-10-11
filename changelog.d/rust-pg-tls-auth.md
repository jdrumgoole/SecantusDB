### Rust PostgreSQL server: client certificates, channel binding and `pg_stat_ssl`

The Rust PostgreSQL server could offer TLS, and that was all it could do
with it. A client that required SCRAM channel binding gave up, a client
certificate was never asked for, and nothing could make the server refuse a
connection in the clear. It has no `pg_hba.conf`, so three options now stand
for the lines of one that matter here.

Each behaviour was measured against PostgreSQL 15.19 in the matching
`pg_hba.conf` configuration: 154 scenarios through psycopg 3.3.4 and a
hand-written SCRAM client, of which 150 answer the same. The four that
differ are one known case: this server trusts a user it has never heard of.

#### Added

- **SCRAM channel binding.** A password login over TLS is offered
  `SCRAM-SHA-256-PLUS` (`tls-server-end-point`), so
  `channel_binding=require` connects. A client that says it can bind but
  was not offered it is refused when the server did offer it, with
  PostgreSQL's SQLSTATE, message and detail.
- **`--tls-ca-file`** (`TlsConfig::ca_file`): a TLS client is asked for a
  certificate, and one it sends must be signed by a CA in that file.
- **`--tls-client-cert verify-ca | verify-full | cert`**
  (`TlsConfig::client_cert`): a certificate must be sent; its common name
  must also be the user name; or that, with no password asked for.
- **`--tls-require`** (`TlsConfig::require_tls`): a connection that is not
  TLS is refused with PostgreSQL's "no pg_hba.conf entry ... no encryption".
- **`pg_stat_ssl`**, which answered 42P01: a row for each connection with
  the protocol version, the cipher under OpenSSL's name, and the client
  certificate's subject, serial number and issuer.
- `PgBuilder::tls_config`, which takes a whole `TlsConfig`.
- `pg_backend_pid()` inside an expression (`where pid = pg_backend_pid()`).
  It worked only as a select-list item on its own.

#### Changed

- Selecting a SASL mechanism the server did not offer is 08P01 "client
  selected an invalid SASL authentication mechanism", as PostgreSQL has it.
  It was 28000 with other words.
- A SCRAM client-final message whose `c=` attribute does not repeat the
  header of the first message is refused. It was not looked at.
- `TlsConfig` is `#[non_exhaustive]`: build it with `TlsConfig::new`.
