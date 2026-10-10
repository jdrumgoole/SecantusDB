Title: secantus-pg 0.1.0-beta.9: TLS
Date: 2026-10-10 23:00:00
Slug: secantus-pg-0-1-0-beta-9
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.9 offers TLS. Given a certificate and its key, the Rust PostgreSQL server completes the handshake a client asks for, and psycopg gets the same answer from it as from PostgreSQL 15.19 for every `sslmode`. Channel binding and client certificates are not in it.

Until this release the Rust PostgreSQL server answered every TLS request
with "not supported". An application whose connection string says
`sslmode=require` could not use it for its tests without changing that
string. This release is the fix for that.

## Starting it with a certificate

```sh
secantusd-pg ./pg-data 127.0.0.1:5432 \
    --tls-cert-file server.crt --tls-key-file server.key
```

Both files are PEM. The certificate file holds the server's certificate
first and any intermediates after it. The key is PKCS#8, PKCS#1 or SEC1,
with no passphrase.

Embedded in a Rust test, the builder takes the same pair:

```rust
let server = secantus_pg::PgServer::builder()
    .tls("server.crt", "server.key")
    .start()?;
```

A client that asks for TLS gets it. A client that does not ask is still
served in the clear, as PostgreSQL serves one under a `host` line in
`pg_hba.conf`. Without the two options nothing changes: the server answers a
TLS request with "not supported", which is PostgreSQL with `ssl = off`.

A server that cannot use its certificate does not start. A missing file, a
key that belongs to another certificate, or one option of the pair without
the other is an error with the file named in it. Starting anyway would serve
in the clear to every client that only prefers TLS, which is libpq's default.

## Against PostgreSQL 15.19

We ran psycopg 3.3.4 (libpq 18) against this server and against PostgreSQL
15.19 with `ssl = on`, both holding the same certificate. They answer the
same for:

- `sslmode=disable`, `prefer`, `require`, `verify-ca` and `verify-full`;
- a certificate signed by a CA the client does not trust, which the client
  refuses;
- a client that allows nothing newer than TLS 1.2;
- a SCRAM-SHA-256 login over TLS, with the right password and the wrong one;
- `SHOW ssl`, which reads `on` and stays `on` after `RESET ALL`, and
  `SET ssl`, which is refused with 55P02.

They differ in two places.

- **Channel binding.** A client with `channel_binding=require` and a
  password connects to PostgreSQL, which offers `SCRAM-SHA-256-PLUS`. This
  server does not offer it, and libpq gives up. The default,
  `channel_binding=prefer`, connects to both.
- **Direct TLS.** A client with `sslnegotiation=direct` connects to this
  server. That is what PostgreSQL 17 does; 15.19 drops the connection.

## What is not in it

- Client certificates: no `clientcert=verify-ca` or `verify-full`, and no
  `cert` authentication.
- A way to refuse a connection that is not encrypted.
- `pg_stat_ssl`, which cannot be read.
- A key with a passphrase, and reloading the files without a restart.

## One more change

An option the binary does not know is now an error. `secantusd-pg --tls-cert
server.crt` used to take `--tls-cert` as the storage path and start a server
over a directory of that name, in the clear.

## psycopg's own tests

psycopg 3.3.4's unmodified suite, run on macOS against this release's code:
of the 5,731 tests that ran, 5,544 pass and none fail.
That suite runs with no certificate, so it does not exercise TLS; the
comparison above does.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.9` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.9)
