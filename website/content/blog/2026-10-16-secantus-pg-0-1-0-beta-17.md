Title: secantus-pg 0.1.0-beta.17: privileges a cursor skipped, parameters read as numbers, and the login rules PostgreSQL has
Date: 2026-10-16 12:00:00
Slug: secantus-pg-0-1-0-beta-17
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.17 fixes answers that were wrong without an error: a cursor or a TABLESAMPLE that read a table the role had no privilege on, a GRANT inside a transaction that took no effect until COMMIT, and string parameters in a VALUES list read as numbers. It adds most of the pg_hba.conf and TLS rules PostgreSQL has, 256 more of PostgreSQL's built-in functions, and more of its ICU collations.

This release is seven batches of the Rust PostgreSQL server's backlog. Every
change was found by running the same statement on this server and on
PostgreSQL 15.19, and each is now checked against PostgreSQL's answer.

## Answers that were wrong without an error

- **A cursor's query and a `TABLESAMPLE` skipped the privilege checks.**
  `DECLARE c CURSOR FOR SELECT * FROM t` read a table the role had no
  `SELECT` on, and read columns a column `GRANT` did not cover, and so did
  `SELECT * FROM t TABLESAMPLE SYSTEM (100)`. Row-level security was skipped
  the same way. Each is refused with PostgreSQL's 42501 now.
- **`GRANT` and `REVOKE` inside `BEGIN ... COMMIT` took no effect until the
  commit.** The privilege check read committed state, so a role revoked in a
  block could still read in it. They apply to the statements that follow
  them in the block now, as in PostgreSQL.
- **String parameters in a `VALUES` list were read as numbers.** psycopg
  sends a Python `str` without a type, and the server read `'1'` as an
  integer. Beside `'a'` and `'b'`, `string_agg` dropped it, `array_agg` made
  it NULL and `ORDER BY` did not sort. PostgreSQL types such a column as
  `text`, and so does this server now. An `INSERT`'s own `VALUES` still
  takes each column's type.
- **A table's row type kept the shape it was created with.** After `ALTER
  TABLE ... ADD COLUMN`, a record literal of the table's type was refused,
  and after `DROP COLUMN` it was read with the dropped column's type.

## Logging in as PostgreSQL does

A `pg_hba.conf` given with `--hba-file` now reads `samehost` and `samenet`
(by this host's interfaces), host names (by a reverse lookup confirmed
forward, as PostgreSQL does), `@file` inclusion, and `+role` without counting
a superuser as a member of every role. A `pg_ident.conf` reads `@file` too,
and its patterns are regular expressions as the SQL `~` operator reads them.

On the TLS side: a key encrypted with a passphrase, given by
`--tls-key-passphrase-command` as PostgreSQL's `ssl_passphrase_command`
gives it, in PKCS#8 or the older OpenSSL PEM form; revocation lists scoped by
an issuing distribution point, and delta lists; a direct-TLS connection
closed as PostgreSQL 15 closes one; an expired password that runs the
password exchange before it fails, so a client cannot tell it apart from a
wrong one; and `SIGHUP`, which reloads and never stops the server.

`inet_client_addr()`, `inet_server_addr()` and their `_port` functions
answer the connection's addresses.

All of this is checked by three login comparisons with PostgreSQL 15, 915
scenarios in all, which run in CI.

## PostgreSQL's built-in functions

The function-coverage probe calls every immutable or stable function in
PostgreSQL 15's catalog on both servers. It went from 1,634 to 1,890 of
1,934 answering as PostgreSQL does: the types' output and send functions,
the B-tree and hash support functions, the aggregates' transition and final
functions, the byte comparison functions, and functions that take
`VARIADIC` arguments. `json` keeps its own text through `->` and `#>`, and
the LATIN encodings convert.

## ICU collations

`pg_collation` lists 878 of PostgreSQL 15's 907 ICU collations, each checked
to sort as PostgreSQL does over 278 strings in 25 scripts. `az-Cyrl`,
`hi-Latn`, `pa-Arab`, `uz-Arab` and `uz-Cyrl` sort as their language does,
the way PostgreSQL's ICU falls back. Marathi, Assamese, Lingala and Konkani,
which a wider check found sorting differently from PostgreSQL, are refused
by name rather than answering in the wrong order.

## The checks that ran

- All 318 statement corpora against PostgreSQL 15.19, one at a time: no
  statement differs.
- The three login comparisons, 234, 547 and 134 scenarios, on macOS and in
  CI on Linux.
- psycopg 3's own test suite: 5,543 passed. The one failure was the
  client-side `test_leak` object count, which passed three times in a row
  when run again.
- The release workflow's smoke test, through a restart, on both binaries
  before they were attached.
- `cargo install secantus-pg --version 0.1.0-beta.17` from crates.io, and a
  query against what it built.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.17` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.17)
