# The two servers

SecantusDB ships **two separate servers** that speak the same MongoDB wire
protocol. You run **one or the other** — there is no in-process engine
switching, and a client never sees a mix of the two.

- **The Rust server** — the flagship: a self-contained Rust server (its own
  wire / dispatch / cursors / accept loop over the pure-Rust engines and a
  WiredTiger-backed store) that runs its accept loop off the GIL. It ships in
  the `SecantusDB` wheel as an embedded lifecycle handle (`RustServer`) and
  as the standalone `secantusd-rs` binary; Python is only the launcher, never
  in the request path.
- **The Python server** — the original pure-Python `SecantusDBServer`. It is
  the readable reference: every operator, stage and error message lands here
  first.

Each is held to **`mongod`**, never to the other. Comparing the two servers
to each other detects drift; it never says which one is right.

Both store data on the same vendored **WiredTiger** engine `mongod` ships, so
the on-disk durability story is identical. The difference is the layers above
storage — command dispatch, query planning, the operator engines — which are
Python in one server and Rust in the other.

:::{note}
The old in-process accelerator (`SECANTUS_ENGINE=rust` /
`SecantusDBServer(engine=...)`) has been **retired** in favour of this
two-server split. The Python server is always pure-Python; the Rust engines
live only in the Rust server.
:::

## Which one should I use?

| | Rust server | Python server |
| --- | --- | --- |
| Package | the `secantus-mdb` crate (crates.io), or a release archive | `pip install SecantusDB` |
| Run it as | `secantusd-rs` / `secantus_mdb::Server` | `SecantusDBServer` / `secantusd-py` |
| Conformance | **99.5%** of pymongo's own suite | see the [validation report](validation-report.md) |
| Speed | within 1.0×–3.2× of `mongod` per operation | 2×–27× |
| Request path | pure Rust (off the GIL) | pure Python |

Use the **Rust server** to run SecantusDB — in tests, in CI, in a container.
Reach for the **Python server** when you want to read how something works, or
need one of the few features only it has (listed below and in the
[Feature comparison](feature-comparison.md)). Speed figures are from
[Benchmark](benchmark.md).

The same split exists on the PostgreSQL side: the Rust PostgreSQL server
(`secantusd-pg`) and the Python one
(`SecantusPGServer` / `secantusd-py-pg`). See the
[SQL / PostgreSQL interface](sql.md).

## Versioning

The two servers are **separate deliverables on independent version lines**;
they diverged at `0.5.2` and advance independently:

- **Python server** — `0.6.0bN` (PEP 440). This is the **PyPI package** version
  in `pyproject.toml` / `secantus.__version__`.
- **Rust server** — `0.5.3-beta.N` (SemVer pre-release), carried in lockstep
  across the `crates/*` workspace and surfaced over the wire as
  `buildInfo.secantusVersion`, by the `secantusd-rs --version` flag, and by
  the embedded handle's `RustServer.version`.

A change that touches only one server bumps only that server's version.

## Running each server

### Python server

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=27017) as server:
    client = MongoClient(server.uri)
    client["mydb"]["users"].insert_one({"_id": 1, "name": "Joe"})
```

Or as a daemon — `pip install` puts a `secantusd-py` script on `PATH`:

```bash
secantusd-py --host 127.0.0.1 --port 27017
```

See [Quickstart](quickstart.md) and [Installation](installation.md).

### Rust server

The Rust server ships as the `secantus-mdb` crate on crates.io, not in the
Python wheel. `cargo install` builds it (WiredTiger included) and puts the
`secantusd-rs` daemon on `PATH`:

```bash
cargo install secantus-mdb --version 0.5.3-beta.188
secantusd-rs --host 127.0.0.1 --port 27017
```

From a Rust test, the same crate starts the server in-process as
`secantus_mdb::Server`. From a Python test, run `secantusd-rs` as a
subprocess and point `pymongo` at it, or use the Python server, which has
the same `pymongo` surface.

Standalone `secantusd-rs` archives (no Python needed) are attached to the
`secantusdb-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases). Both
Mongo daemons read the same `secantusd.toml` config (see
[Configuration](configuration.md)).

### SQL / PostgreSQL servers

The Rust PostgreSQL server is the `secantus-pg` crate on crates.io (`cargo
install secantus-pg --version 0.1.0-beta.13`), and also ships as a standalone
`secantusd-pg` archive on
the `secantusd-pg-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases):

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432
```

To offer TLS, give it a PEM certificate chain and its private key (no
passphrase):

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432 \
    --tls-cert-file server.crt --tls-key-file server.key
```

A client that asks for TLS then gets it (`sslmode=require`, `verify-ca`,
`verify-full`); one that does not ask is still served in the clear. Without
the pair the server answers a TLS request with "not supported", as PostgreSQL
does with `ssl = off`. These two options are in 0.1.0-beta.9 and later.

A password login over TLS is offered SCRAM channel binding
(`SCRAM-SHA-256-PLUS`), so a client with `channel_binding=require` connects.
Three more options stand in for the `pg_hba.conf` this server does not have.
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
default, is read. A list with a critical extension (one scoped by a
distribution point, a delta list) stops the server from starting.

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

A line this server cannot apply stops it from starting, with the line's
number, so that no rule is silently skipped: a method that needs a service
outside the server (`peer`, `ident`, `gss`, `sspi`, `ldap`, `radius`, `pam`,
`bsd`), a host name for the address, `samenet`, and `@file` inclusion.
`--hba-file` cannot be given with `--tls-require`, `--tls-client-cert`,
`--tls-client-name`, `--tls-user-map` or `--refuse-unknown-users`: the file
says what those say.

A password file (`~/.pgpass`, or libpq's `passfile`) works as it does with
PostgreSQL. It is read by the client library, which sends the password it
finds there; the server sees an ordinary password login.

A user name there is no role for connects without a password, as a role
with no password does. `--refuse-unknown-users` (since 0.1.0-beta.11) stops
that: such a user is asked for a password, as PostgreSQL asks one under a
`scram-sha-256` line, and none it gives is right. A role with no password
still connects without one, so give `postgres` a password
(`ALTER ROLE postgres PASSWORD '...'`) and every login then needs one.

The Python one needs the `sql` extra (`pip install "SecantusDB[sql]"`) and
runs as `secantusd-py-pg`:

```bash
secantusd-py-pg --host 127.0.0.1 --port 5432 --storage-path ./secantus-data
```

See the [SQL / PostgreSQL interface](sql.md).

## What each server does **not** support

Both servers share the project-wide non-goals — anything that depends on **real
cluster topology** (multi-node replica sets, sharding, elections, cross-node
oplog), auth mechanisms beyond SCRAM (SHA-1 / SHA-256) and `MONGODB-X509`,
`OP_COMPRESSED`, text / hashed / wildcard indexes, and
`$where` / `$function` / `$accumulator` / JS `mapReduce` (no embedded JS
runtime).
These are out of scope for **both** servers; the per-feature detail lives in
[Compatibility](compatibility.md).

### Python server

The Python server implements the full in-scope wire surface. Its remaining
divergences are the stopgaps and known edge cases enumerated in
[Compatibility](compatibility.md) — the `_id` numeric-type bridge is
undefined for `NaN` / infinity, `top` counters are always zero, and a handful
of date-format and
`$group`-ordering edge cases. There is no *feature* the Python server is
missing relative to the in-scope set; it is the conformance reference the Rust
server is measured against.

### Rust server

The Rust server passes 99.5% of pymongo's suite. The remaining *feature* differences (full three-way matrix in the
[Feature comparison](feature-comparison.md)) are:

- **Point-in-time restore over the wire** — `secantusAdmin.restoreToTimestamp`
  is Python-server-only; the Rust server does the same restore via the
  `secantusd-rs restore` CLI subcommand.
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
