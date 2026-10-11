### Rust PostgreSQL server: `pg_hba.conf`

The Rust PostgreSQL server applied one login rule to every connection, set
by command-line options. `--hba-file` takes a `pg_hba.conf` in their place,
read as PostgreSQL 15 reads it.

#### Added

- `--hba-file` (`PgBuilder::hba_file`, `ServerOptions::hba_file`). The first
  line whose type, database, user and address match a connection decides
  it, and a connection no line matches is refused with PostgreSQL's "no
  pg_hba.conf entry". Read: `host`, `hostssl`, `hostnossl`; `all`,
  `sameuser`, `samerole`, names and lists for the database; `all`, names,
  `+role` and lists for the user; `all`, `samehost`, and an address with a
  prefix length or a mask; the `trust`, `reject`, `scram-sha-256`, `md5`,
  `password` and `cert` methods; `clientcert=`, `clientname=` and `map=`.
- `--ident-file` (`PgBuilder::ident_file`), the `pg_ident.conf` a `map=` is
  looked up in.
- The `password` method: the password in the clear, checked against a
  stored SCRAM verifier or md5 hash.
- The file is read again on SIGHUP, `select pg_reload_conf()` and
  `PgServer::reload`.

#### Changed

- With a `pg_hba.conf`, a role with no password cannot pass a
  `scram-sha-256` line and a user name with no role is refused, as on
  PostgreSQL. Without one, both still connect, as before.
- With a `pg_hba.conf`, a login is proved before the database is looked up:
  a wrong password for a database that is not there is a wrong password.
- `PgServer::reload_tls` and `TlsReloader`, added earlier in this release
  cycle and not yet released, are `PgServer::reload` and `Reloader`.

A line the server cannot apply stops it from starting, with the line's
number: a method that needs an outside service (`peer`, `ident`, `gss`,
`sspi`, `ldap`, `radius`, `pam`, `bsd`), a host name for the address,
`samenet`, and `@file` inclusion.

Run against PostgreSQL 15.19 with the same two files: 118 of 118 scenarios
answer the same.
