Title: secantus-pg 0.1.0-beta.10: client certificates and channel binding
Date: 2026-10-11 14:00:00
Slug: secantus-pg-0-1-0-beta-10
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.10 lets the Rust PostgreSQL server say what a TLS connection must show: a client certificate, one that names the user, or one in place of the password. A password login over TLS is offered SCRAM channel binding, the server can refuse a connection in the clear, and `pg_stat_ssl` can be read. Of 154 login scenarios run against PostgreSQL 15.19, 150 answer the same.

The last release gave the Rust PostgreSQL server TLS, and its note listed
what TLS could not yet do: check a client certificate, bind a password login
to the channel, or refuse a connection that is not encrypted. This release
is those three.

PostgreSQL decides who may connect, and how, in `pg_hba.conf`. This server
has no such file. Three options stand for the lines of one that matter here.

## Client certificates

```sh
secantusd-pg ./pg-data 127.0.0.1:5432 \
    --tls-cert-file server.crt --tls-key-file server.key \
    --tls-ca-file ca.pem --tls-client-cert verify-full
```

`--tls-ca-file` is PostgreSQL's `ssl_ca_file`. A TLS client is asked for a
certificate, and one it sends must be signed by a CA in that file. On its
own it requires nothing: a client that sends no certificate still connects.

`--tls-client-cert` says what must be sent.

| Mode | What the client must show | PostgreSQL's equivalent |
| --- | --- | --- |
| `verify-ca` | A certificate the CA signed. | `clientcert=verify-ca` |
| `verify-full` | A certificate whose common name is the user name. A role with a password still gives it. | `clientcert=verify-full` |
| `cert` | The same certificate, and no password is asked for. | The `cert` method |

Each mode needs `--tls-ca-file`, and each refuses a connection that is not
TLS, as PostgreSQL allows `clientcert` on a `hostssl` line only.

Under `verify-full` the name is checked after the password, and a failure is
reported as the method that let the login in. A user with the right password
and somebody else's certificate reads "password authentication failed".
That is PostgreSQL's order and PostgreSQL's message, and it is what this
server answers.

Embedded in a Rust test, the builder takes the same choices:

```rust
use secantus_pg::{ClientCert, PgServer, TlsConfig};

let tls = TlsConfig::new("server.crt", "server.key")
    .ca_file("ca.pem")
    .client_cert(ClientCert::VerifyFull);
let server = PgServer::builder().tls_config(tls).start()?;
```

## Channel binding

A password login over TLS is now offered `SCRAM-SHA-256-PLUS`. A client set
to `channel_binding=require` connects; before this release its own library
refused, because the server offered nothing to bind to.

Binding proves the client is talking to the holder of the server
certificate and not to something relaying the exchange. The check that
makes it worth having is the refusal: a client that says "I can bind, but
you did not offer it" on a connection where the server did offer it is
turned away, because something between the two removed the offer.

## Refusing a connection in the clear

`--tls-require` refuses a connection that is not TLS, with the message
PostgreSQL gives when its only `pg_hba.conf` lines are `hostssl`:

```
FATAL:  no pg_hba.conf entry for host "127.0.0.1", user "app", database "app", no encryption
```

libpq's default, `sslmode=prefer`, asks for TLS first, so it connects.

## `pg_stat_ssl`

`pg_stat_ssl` answered "relation does not exist". It now has a row for each
connection: whether it is TLS, the protocol version, the cipher under
OpenSSL's name, and the client certificate's subject, serial number and
issuer.

```sql
select version, cipher, bits, client_dn from pg_stat_ssl where pid = pg_backend_pid();
--  TLSv1.3 | TLS_AES_256_GCM_SHA384 | 256 | /O=Example/CN=alice
```

That query also needed `pg_backend_pid()` to work inside a `WHERE`. It
worked only as a select-list item on its own.

## Against PostgreSQL 15.19

We ran the same logins against this server and against PostgreSQL 15.19 in
five configurations: any connection allowed, TLS only, and the three
client-certificate modes. There were 154 scenarios, through psycopg 3.3.4
and through a client written by hand to send the SCRAM messages libpq never
sends. 150 answer the same, SQLSTATE, message and detail.

The four that differ are one case. A user this server has never heard of
connects without a password; PostgreSQL asks it for one. That is on the
page's warning already and has not changed. Under `verify-full` and `cert`
such a user must still hold a certificate that names it.

## What is not in it

- One policy for every user and database. `pg_hba.conf` chooses by line.
- A certificate revocation list, a user name map, and matching on the
  certificate's whole DN. The common name must be the user name.
- Channel binding for a server certificate signed with RSA-PSS or Ed25519.
- A key with a passphrase, and reloading the files without a restart.

## Two smaller changes

- Selecting a SASL mechanism the server did not offer is refused with
  PostgreSQL's SQLSTATE and message, 08P01. It was 28000 with other words.
- `TlsConfig` can no longer be built as a struct literal. Use
  `TlsConfig::new`.

## psycopg's own tests

psycopg 3.3.4's unmodified suite, run on macOS against this release's code:
of the 5,731 tests that ran, 5,544 pass and none fail, the same counts as
0.1.0-beta.9. That suite runs with no certificate, so it does not exercise
TLS; the comparison above does.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.10` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.10)
