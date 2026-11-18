# The servers

SecantusDB is **two single-node database servers**, one per wire protocol,
both written in Rust over the same WiredTiger storage engine `mongod` ships.
You run one or both; each is its own process with its own data directory.

| | MongoDB server | PostgreSQL server (beta) |
| --- | --- | --- |
| Binary | `secantusd-mdb` | `secantusd-pg` |
| Crate | [`secantus-mdb`](https://crates.io/crates/secantus-mdb) | [`secantus-pg`](https://crates.io/crates/secantus-pg) |
| Library | `secantus_mdb::Server` | `secantus_pg::PgServer` |
| Wire protocol | MongoDB | PostgreSQL |
| Held to | `mongod` 8.x | PostgreSQL |
| Conformance | **99.5%** of pymongo's own suite | psycopg 3's own suite, unmodified |
| Speed | within 1.0×–3.2× of `mongod` per operation | see [Benchmark](benchmark.md) |
| Get it | PyPI, crates.io, GitHub Releases | PyPI, crates.io, GitHub Releases |

Each is held to the real product — **`mongod`** or **PostgreSQL** — never to
the other server. See [Installation](installation.md) for the three ways to
get them, and [Quickstart](quickstart.md) to run one.

Both store data on the vendored **WiredTiger** engine `mongod` ships, so the
on-disk durability story is the same. A table `secantusd-pg` creates reads back
through `secantusd-mdb` as a collection, with the `PRIMARY KEY` as `_id`; the
reverse does not work, and the two cannot serve the same directory at the same
time (WiredTiger's exclusive file lock makes the second server exit).

:::{note}
The original Python servers (`secantus.SecantusDBServer`, `secantusd-py`,
`secantusd-py-pg`) are **fully deprecated**: not tested in CI, not published,
no new work. They are documented, as a record, on
[Deprecated: Python servers](python-servers.md). The old in-process
accelerator (`SECANTUS_ENGINE=rust` / `SecantusDBServer(engine=...)`) was
retired earlier.
:::

## Versioning

The servers are **separate deliverables on independent version lines**:

- **MongoDB server** — `0.5.3-beta.N` (SemVer pre-release), carried in lockstep
  across the MongoDB crates and surfaced over the wire as
  `buildInfo.secantusVersion` and by `secantusd-mdb --version`.
- **PostgreSQL server** — `0.1.0-beta.N`, on its own line
  (`secantusd-pg --version`).
- **The PyPI wheel** — versioned as the MongoDB server (`0.5.3-beta.196` is
  `0.5.3b196`); it bundles the newest released `secantusd-pg` alongside.

A change that touches only one server bumps only that server's version.

## Running each server

### MongoDB server

```bash
pip install SecantusDB      # prebuilt secantusd-mdb and secantusd-pg on PATH
# or: cargo install secantus-mdb --version 0.5.3-beta.198
secantusd-mdb --host 127.0.0.1 --port 27017 --storage-path ./secantus-data
```

From Rust, `secantus_mdb::Server` starts the same server in-process; from
Python, `secantusdb.MongoServer` starts the binary as a subprocess. Standalone
`secantusd-mdb` archives are attached to the `secantusdb-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases). The
daemon reads a `secantusd.toml` config (see [Configuration](configuration.md))
and its own docs are in the
[Rust server docs](https://secantusdb.com/docs/rust/index.html).

### PostgreSQL server

The PostgreSQL server is the `secantus-pg` crate on crates.io (`cargo
install secantus-pg --version 0.1.0-beta.20`), is in the PyPI wheel, and also ships as a standalone
`secantusd-pg` archive on
the `secantusd-pg-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases):

```bash
pip install SecantusDB      # the same wheel carries secantusd-pg
secantusd-pg ./secantus-pg-data 127.0.0.1:5432
```

To offer TLS, give it a PEM certificate chain and its private key:

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432 \
    --tls-cert-file server.crt --tls-key-file server.key
```

A client that asks for TLS then gets it (`sslmode=require`, `verify-ca`,
`verify-full`); one that does not ask is still served in the clear. Without
the pair the server answers a TLS request with "not supported", as PostgreSQL
does with `ssl = off`. These two options are in 0.1.0-beta.9 and later.

An encrypted key -- PKCS#8 (`-----BEGIN ENCRYPTED PRIVATE KEY-----`), or
OpenSSL's legacy form (`Proc-Type: 4,ENCRYPTED`, AES or DES-EDE3 in CBC
mode) -- needs `--tls-key-passphrase-command CMD`, PostgreSQL's
`ssl_passphrase_command`: the first line the shell command prints is the
passphrase, `%p` in it is the prompt and `%%` a `%`. It runs again on each
reload.

A password login over TLS is offered SCRAM channel binding
(`SCRAM-SHA-256-PLUS`), so a client with `channel_binding=require` connects.
Three more options stand in for a `pg_hba.conf`, for a server that has none.
Channel binding and these options are in 0.1.0-beta.10 and later.

| Option | What it does | PostgreSQL's equivalent |
| --- | --- | --- |
| `--tls-require` | Refuses a connection that is not TLS. | A `pg_hba.conf` with only `hostssl` lines |
| `--tls-ca-file ca.pem` | Asks a TLS client for a certificate. One it sends must be signed by a CA in this PEM file; a client that sends none still connects. | `ssl_ca_file` |
| `--tls-client-cert verify-ca` | A certificate must be sent. | `clientcert=verify-ca` |
| `--tls-client-cert verify-full` | A certificate must be sent and its common name must be the user name. A role with a password still gives it. | `clientcert=verify-full` |
| `--tls-client-cert cert` | The same check, and no password is asked for. | The `cert` method |

`--tls-client-cert` needs `--tls-ca-file`, and it refuses a connection that
is not TLS, as PostgreSQL allows `clientcert` on a `hostssl` line only.

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432 \
    --tls-cert-file server.crt --tls-key-file server.key \
    --tls-ca-file ca.pem --tls-client-cert verify-full
```

`pg_stat_ssl` has a row for each connection: whether it is TLS, the protocol
version, the cipher under OpenSSL's name, and the client certificate's
subject, serial number and issuer.

Four more, since 0.1.0-beta.12, say which certificates are accepted and
which roles they may log in as:

| Option | What it does | PostgreSQL's equivalent |
| --- | --- | --- |
| `--tls-crl-file crl.pem` | Refuses a client certificate that a revocation list in this PEM file names. | `ssl_crl_file` |
| `--tls-client-name DN` | Compares the certificate's whole subject with the user name, in place of its common name. With `verify-full` or `cert`. | `clientname=DN` |
| `--tls-ident-file pg_ident.conf` | A user name map file. With `--tls-user-map`. | `ident_file` |
| `--tls-user-map NAME` | Under `cert`, a certificate logs in as the roles the map `NAME` gives it, and only those. | `map=NAME` |

With a revocation list, every certificate in a client's chain needs a list
from its issuer that is in date, as OpenSSL requires of PostgreSQL: a
certificate whose CA has no list in the file, or only an expired one, is
refused. A list with no extensions, which `openssl ca -gencrl` writes by
default, is read. A list scoped by an issuing distribution point applies
only to the certificates it covers, and a delta list is read as a full one,
as OpenSSL applies them for PostgreSQL. A list with any other critical
extension stops the server from starting.

The subject under `--tls-client-name DN` is written as RFC 2253 has it, the
last attribute first: `CN=alice,O=Example`. That is seldom a user name, so
it goes with a map:

```
# pg_ident.conf: map name, what the certificate says, role
staff   "/^CN=(.*),O=Example$"   \1
staff   build-agent              ci
```

A line whose second field begins with `/` is a regular expression, and `\1`
in the role is what its first group matched. The expressions are the Rust
`regex` crate's, which agree with PostgreSQL's on what such a map is
written with; one that does not compile stops the server from starting.

The TLS files are read again on SIGHUP, and on `select pg_reload_conf()`
from a superuser: the certificate and key, the CA file, the revocation list
and the map. A connection made from then on gets what they hold. When they
cannot be used the server logs why and keeps what it had. SIGHUP reloads
only on a server started with TLS; without TLS it stops the server, as
before.

A certificate whose signature is RSA-PSS is bound to with the hash its
parameters name. One signed with Ed25519 names no hash: channel binding is
offered, as PostgreSQL offers it, and a login that takes it up fails with
"could not find digest for NID UNDEF". libpq takes it up by default, so a
client of such a server needs `channel_binding=disable`.

Not supported: a key with a passphrase.

### A `pg_hba.conf`

The options above apply one rule to every connection. `--hba-file` (since
0.1.0-beta.12) takes a `pg_hba.conf` in their place, and its lines decide
each login as PostgreSQL's do: the first line whose type, database, user
and address match the connection says how it must prove who it is, and a
connection no line matches is refused.

```
# TYPE     DATABASE  USER      ADDRESS        METHOD
host       all       postgres  127.0.0.1/32   trust
hostssl    appdb     +staff    10.0.0.0/8     scram-sha-256
hostssl    all       agent     all            cert map=agents
host       all       all       all            reject
```

```bash
secantusd-pg ./secantus-pg-data 0.0.0.0:5432 \
    --tls-cert-file server.crt --tls-key-file server.key --tls-ca-file ca.pem \
    --hba-file pg_hba.conf --ident-file pg_ident.conf
```

| Part of a line | What is read |
| --- | --- |
| Type | `host`, `hostssl`, `hostnossl`. A `local` line is read and never matches: there is no Unix-domain socket. |
| Database | `all`, `sameuser`, `samerole`, a name, or a list of these. |
| User | `all`, a name, `+role` for a role and its members, or a list. |
| Address | `all`, `samehost`, an address with a prefix length (`10.0.0.0/8`), or an address and a mask. |
| Method | `trust`, `reject`, `scram-sha-256`, `md5`, `password`, `cert`. |
| Options | `clientcert=verify-ca` or `verify-full`, `clientname=CN` or `DN`, and `map=` with `--ident-file`. |

With a file, a role that has no password cannot pass a `scram-sha-256` line,
and a user name with no role is refused, both as on PostgreSQL. The file is
read again on SIGHUP and on `select pg_reload_conf()`; one that cannot be
read leaves the old one in force.

Without `--hba-file`, a `pg_hba.conf` in the data directory (the store's
directory) is read in its place, with the `pg_ident.conf` beside it, as
PostgreSQL reads its default files; `SHOW hba_file` and `ident_file` name
them. A store with no such file is served by the options above.

The data directory's `postgresql.conf`, when it has one, is read too, with
what its `include`, `include_if_exists` and `include_dir` lines name, below
the `postgresql.auto.conf` that `ALTER SYSTEM` writes. `pg_file_settings`
lists both, and `select pg_reload_conf()` puts a change in force.

A line this server cannot apply stops it from starting, with the line's
number, so that no rule is silently skipped: a method that needs a service
outside the server (`peer`, `ident`, `gss`, `sspi`, `ldap`, `radius`, `pam`,
`bsd`). A host name for the address is matched by a reverse lookup of the
client's address confirmed forward, as PostgreSQL's is. `@file` inclusion reads a file of
names beside the `pg_hba.conf`, as PostgreSQL 15 does. `samehost`
and `samenet` match by this host's interface addresses and their networks,
as PostgreSQL's do.
`--hba-file` cannot be given with `--tls-require`, `--tls-client-cert`,
`--tls-client-name`, `--tls-user-map` or `--trust-unknown-users`: the file
says what those say.

A password file (`~/.pgpass`, or libpq's `passfile`) works as it does with
PostgreSQL. It is read by the client library, which sends the password it
finds there; the server sees an ordinary password login.

A user name there is no role for is refused, as PostgreSQL refuses a role
that does not exist: it is asked for a password, as PostgreSQL asks one
under a `scram-sha-256` line, and none it gives is right. Connect as
`postgres`, create the role first, or start the server with
`--trust-unknown-users` (`trust_unknown_users=True` on the embedded
handles), which lets such a user in without a password. A role with no
password connects without one, so give `postgres` a password
(`ALTER ROLE postgres PASSWORD '...'`) and every login then needs one.
Until 0.1.0-beta.17 unknown users were trusted by default and
`--refuse-unknown-users` refused them; that flag is still accepted, and
changes nothing.

See the [SQL / PostgreSQL interface](sql.md).

## What each server does **not** support

Both servers share the project-wide non-goals — anything that depends on **real
cluster topology** (multi-node replica sets, sharding, elections, cross-node
oplog), auth mechanisms beyond SCRAM (SHA-1 / SHA-256) and `MONGODB-X509`,
text / hashed / wildcard indexes, and
`$where` / `$function` / `$accumulator` / JS `mapReduce` (no embedded JS
runtime).
These are out of scope for **both** servers; the per-feature detail lives in
[Compatibility](compatibility.md).

### Python server (deprecated)

The deprecated Python server implements the full in-scope wire surface. Its remaining
divergences are the stopgaps and known edge cases enumerated in
[Compatibility](compatibility.md) — the `_id` numeric-type bridge is
undefined for `NaN` / infinity, `top` counters are always zero, and a handful
of date-format and
`$group`-ordering edge cases. It does not speak `OP_COMPRESSED`, which the
Rust server does (`snappy`, `zstd`, `zlib`); a driver asked to compress
simply sends uncompressed to it.

### Rust MongoDB server

The Rust MongoDB server passes 99.5% of pymongo's suite. The remaining *feature* differences (full three-way matrix in the
[Feature comparison](feature-comparison.md)) are:

- **Point-in-time restore over the wire** — `secantusAdmin.restoreToTimestamp`
  is Python-server-only; the Rust server does the same restore via the
  `secantusd-mdb restore` CLI subcommand.
- **Session lifecycle** — `endSessions` / `refreshSessions` / `killSessions`
  are acknowledged no-ops on the Rust server; the Python server tracks
  sessions with a 30-minute idle TTL.
- **Operator edges** — a handful of `$dateFromString` / `$dateToString`
  format directives, Decimal128 arithmetic edges, and mixed-type sort
  orderings the Rust engine rejects rather than risk diverging from
  `mongod`.
- **Thinner diagnostics** — `serverStatus` / `dbStats` / `collStats` return a
  smaller subset of fields than the Python server's replies.

Both servers mint resume tokens in SecantusDB's own `{s, t, n, k}` layout
rather than mongod's keystring format — tokens round-trip within SecantusDB but
cannot be presented to a real `mongod` (or vice versa).

The current Rust-server pass rate, and the exact set of failing pymongo tests,
is regenerated each run into the
[Rust-server validation report](validation-report-rust-server.md). The gap
against the [Python-server report](validation-report.md) is the canonical,
machine-checked statement of what the Rust server doesn't support yet.
