Title: secantus-pg 0.1.0-beta.11: refusing a user there is no role for
Date: 2026-10-11 23:30:00
Slug: secantus-pg-0-1-0-beta-11
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.11 adds `--refuse-unknown-users`. With it, the Rust PostgreSQL server asks a user name it has no role for to give a password, and the login fails as it does on PostgreSQL. Without it nothing changes. Of 219 login scenarios run against PostgreSQL 15.19, all 219 answer the same with the option on.

The last release's note ended its comparison with one case that differed
from PostgreSQL: a user name the server has never heard of connects without
a password. This release has an option that closes it.

## The option

```sh
secantusd-pg ./pg-data 127.0.0.1:5432 --refuse-unknown-users
```

A user there is no role for is now asked for a password, and the login
fails:

```
$ PGPASSWORD=guess psql -h 127.0.0.1 -U nobody -d postgres
psql: error: connection to server at "127.0.0.1", port 5432 failed: FATAL:  password authentication failed for user "nobody"
```

A client with no password to give stops before it sends anything, with
libpq's own `fe_sendauth: no password supplied`.

That is what PostgreSQL answers under a `scram-sha-256` line in
`pg_hba.conf`. It does not say the role is missing, and neither does this
server: the exchange runs to the end against a password nobody holds, so a
client cannot tell a missing role from a wrong password.

Under `--tls-client-cert cert` no password is asked for. The certificate is
checked first, and a certificate that names the user gets PostgreSQL's
answer for that case:

```
FATAL:  role "nobody" does not exist
```

Embedded in a Rust test, the builder takes the same choice:

```rust
let server = secantus_pg::PgServer::builder()
    .refuse_unknown_users(true)
    .start()?;
```

A caller that binds the server itself has `ServerOptions` and `bind_with`,
which take this option and the TLS configuration together.

## Why it is off by default

Test fixtures connect as roles nobody created. A connection string written
for a real database names its own user, and until now that user connected
to this server without a `CREATE ROLE` first. Refusing it by default would
break those suites on upgrade, so the default has not changed.

## What it does not do

A role that exists and has no password still connects without one, with the
option or without it. `postgres` is such a role on a new store. For every
login to need a password, give it one:

```sql
ALTER ROLE postgres PASSWORD '...';
```

PostgreSQL decides this per line of `pg_hba.conf`: a `trust` line lets a
role in with no password, a `scram-sha-256` line does not. This server has
one rule for every user and database.

## Against PostgreSQL 15.19

The last release ran 154 login scenarios against this server and against
PostgreSQL 15.19 in five configurations: any connection allowed, TLS only,
and the three client-certificate modes. We added 65 for a user with no
role, with and without a password, with and without a certificate, and
through a client written by hand to send the SCRAM messages libpq never
sends. That makes 219.

| | Same answer as PostgreSQL 15.19 |
| --- | --- |
| With `--refuse-unknown-users` | 219 of 219 |
| Without it | 171 of 219 |

The 48 that differ without the option are all the unknown user being let
in. The reference server's `pg_hba.conf` trusts its password-less roles by
name, which is the rule this server applies to them.

## psycopg's own tests

psycopg 3.3.4's unmodified suite, run on macOS against this release's code
with the option off, as the suite expects: of the 5,731 tests that ran,
5,544 pass and none fail, the same counts as 0.1.0-beta.10.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.11` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.11)
