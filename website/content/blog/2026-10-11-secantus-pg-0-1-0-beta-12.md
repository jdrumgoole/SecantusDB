Title: secantus-pg 0.1.0-beta.12: pg_hba.conf, revocation lists and a reload
Date: 2026-10-11 23:50:00
Slug: secantus-pg-0-1-0-beta-12
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.12 reads a `pg_hba.conf`, so the Rust PostgreSQL server can hold each user, database and address to its own login rule. It also refuses a revoked client certificate, maps a certificate to roles through a `pg_ident.conf`, and re-reads all of these files without a restart. Of 817 login scenarios run against PostgreSQL 15.19, all 817 answer the same.

Until this release the Rust PostgreSQL server had one login rule for every
connection, chosen by command-line options. The last three release notes
each ended with a list of what that could not say. This release is most of
that list.

## A `pg_hba.conf`

PostgreSQL decides who may connect, and how, in `pg_hba.conf`. The server
now reads one:

```
# TYPE     DATABASE  USER      ADDRESS        METHOD
host       all       postgres  127.0.0.1/32   trust
hostssl    appdb     +staff    10.0.0.0/8     scram-sha-256
hostssl    all       agent     all            cert map=agents
host       all       all       all            reject
```

```sh
secantusd-pg ./pg-data 0.0.0.0:5432 \
    --tls-cert-file server.crt --tls-key-file server.key --tls-ca-file ca.pem \
    --hba-file pg_hba.conf --ident-file pg_ident.conf
```

The first line whose type, database, user and address match a connection
decides it. A connection no line matches is refused, with PostgreSQL's
message:

```
FATAL:  no pg_hba.conf entry for host "127.0.0.1", user "app", database "postgres", no encryption
```

| Part of a line | What is read |
| --- | --- |
| Type | `host`, `hostssl`, `hostnossl` |
| Database | `all`, `sameuser`, `samerole`, a name, or a list |
| User | `all`, a name, `+role` for a role and its members, or a list |
| Address | `all`, `samehost`, an address with a prefix length, or an address and a mask |
| Method | `trust`, `reject`, `scram-sha-256`, `md5`, `password`, `cert` |
| Options | `clientcert=`, `clientname=`, and `map=` with `--ident-file` |

Two things the options could not do come with it. A role that has no
password cannot pass a `scram-sha-256` line, where without a file it is
trusted. And a user name there is no role for is refused.

A line the server cannot apply stops it from starting, with the line's
number. That covers the methods that need a service outside the server
(`peer`, `ident`, `gss`, `sspi`, `ldap`, `radius`, `pam`, `bsd`), a host
name for the address, `samenet`, and `@file` inclusion. Skipping such a
line would let in a connection the file meant to send elsewhere.

The file takes the place of `--tls-require`, `--tls-client-cert` and
`--refuse-unknown-users`, and cannot be given beside them. Without
`--hba-file` those options work as before.

## Revocation lists

`--tls-crl-file` is PostgreSQL's `ssl_crl_file`. A client certificate that a
revocation list names fails the TLS handshake:

```
SSL error: ssl/tls alert certificate revoked
```

As OpenSSL requires of PostgreSQL, every certificate in a client's chain
then needs a list from its issuer that is in date. A certificate whose CA
has no list in the file is refused, and so is one whose CA's list has
passed its next update.

The TLS library this server uses can check revocation itself, but it reads
only a list that carries a CRL number extension. `openssl ca -gencrl`
writes a list with no extensions unless it is configured to add them, and
PostgreSQL reads that. So the check here is the server's own. It verifies
each list's signature against the CA that issued it, and it reads the lists
OpenSSL writes by default.

## User name maps

Under the `cert` method a certificate logs in as the role its common name
is. A `pg_ident.conf` lets it log in as others:

```
# map     certificate says          role
agents    build-agent               ci
agents    "/^(.*)@example\.com$"    \1
```

`--tls-client-name DN` (`clientname=DN`) hands the map the certificate's
whole subject, written as `CN=alice,O=Example`, in place of the common
name.

## A reload

The certificate, its key, the CA file, the revocation list, the
`pg_hba.conf` and the map are read again on SIGHUP, and on
`select pg_reload_conf()` from a superuser. A connection made afterwards
gets what the files hold; one already open keeps what it has. Files that
cannot be used leave the old ones serving, and the server logs why.

A renewed certificate, or a list with one more name in it, used to need a
restart.

## Channel binding and the certificate's signature

A server certificate whose signature is RSA-PSS was offered no SCRAM
channel binding. It is now bound with the hash its signature names, as
PostgreSQL binds it.

A certificate signed with Ed25519 names no hash. PostgreSQL offers channel
binding for it all the same, and a login that takes the offer up fails with
"could not find digest for NID UNDEF". This server now does the same.
**That is a change a user can meet:** libpq takes the offer up by default,
so a client that connected to a server with an Ed25519-signed certificate
before this release needs `channel_binding=disable` now, as it does against
PostgreSQL.

## Two smaller changes

- SIGHUP reloads a server that was started with TLS or a `pg_hba.conf`. It
  used to stop it. A server started with neither still stops.
- The `password` method, the password sent in the clear, is read from a
  `pg_hba.conf`. Use it only on a `hostssl` line.

## Against PostgreSQL 15.19

Each piece was measured against PostgreSQL 15.19 before it was built, and
the comparison was run again on this release's code. Each scenario is one
login, through psycopg 3.3.4 or through a client written by hand to send
what libpq never sends, and "the same" means the same SQLSTATE, message and
detail, or the same TLS alert.

| Comparison | Scenarios | The same |
| --- | --- | --- |
| The options, as in 0.1.0-beta.11 | 219 | 219 |
| Revocation lists, maps, `clientname=DN`, three kinds of server certificate, the reload | 480 | 480 |
| Two `pg_hba.conf` files | 118 | 118 |

The revocation lists in the second row are current, empty, expired, dated
in the future, from another CA, forged, with and without extensions, and
through an intermediate CA.

## What is not in it

- The `pg_hba.conf` methods that need an outside service, a host name for
  the address, and `samenet`. Each is refused at startup.
- A revocation list scoped by a distribution point, and a delta list.
- A key with a passphrase.
- A user name map's patterns are the Rust `regex` crate's, which agree with
  PostgreSQL's on what such a map is written with.

## psycopg's own tests

psycopg 3.3.4's unmodified suite, run on macOS against this release's code
with no `pg_hba.conf`, as the suite expects: of the 5,731 tests that
ran, 5,544 pass and none fail, the same counts as 0.1.0-beta.11.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.12` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.12)
