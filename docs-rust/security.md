# Security

The Rust server implements the same authentication, authorization, and
transport-security surface as the Python server — the same user records on
disk, the same commands, the same driver-facing behaviour. This page is the
Rust-server operational summary; the concept-level documentation lives in
the main tree's [Authentication](https://secantusdb.com/docs/authentication.html)
page and applies verbatim.

## Authentication

- **SCRAM-SHA-256** (MongoDB's default since 4.0) and **SCRAM-SHA-1**
  (per-user opt-in via the `mechanisms` array).
- **MONGODB-X509** — certificate-subject-DN-as-username over mTLS, via the
  standard `authenticate` command path every driver uses.

Auth is off by default. Flip it on with `--auth` (daemon) or
`require_auth=True` (embedded handle); only the handshake and the SCRAM
exchange then run unauthenticated, everything else returns `Unauthorized`
(code 13) until the connection authenticates.

User management is the standard command set: `createUser` / `updateUser` /
`dropUser` / `dropAllUsersFromDatabase` / `usersInfo`. User records persist
in storage, so the bootstrap pattern is: start without `--auth`, provision
users, restart with `--auth` on the same `--storage-path`.

## Authorization (RBAC)

With auth on, every command is checked against the connection's effective
privileges, which are the union of every role it holds and every role those
inherit. A miss returns mongod's `not authorized on <db> to execute command`
error.

The check is mongod's.

- **Resources match by mongod's rules.** A privilege on one collection
  covers that collection only.
- **Every command needs the actions mongod asks for, and its arguments
  count.** An upsert also needs `insert`. A pipeline needs what each stage
  needs. `explain` needs what it explains. `bulkWrite` is checked one
  operation at a time.
- **A command the server cannot check is refused.**
- **A cursor belongs to the user that opened it.** Another user cannot read
  from it or kill it.
- **Two listings are filtered rather than refused.** `listDatabases` lists
  only the databases a user has a privilege on. `listCollections` with
  `authorizedCollections: true` lists only the collections a user can
  reach.

The built-in roles are mongod's, with the privileges mongod gives each
(`read`, `readWrite`, `dbAdmin`, `userAdmin`, `dbOwner`, the `AnyDatabase`
variants, the cluster roles, `backup`, `restore`, `root` and the rest).
Custom roles work through `createRole` / `updateRole` / `dropRole` /
`rolesInfo` and the grant and revoke commands.

The server's own commands have no mongod rule, so the server sets one:

| command | privilege it needs |
| --- | --- |
| `backupArchive`, `archiveBaseSnapshot` | the cluster privilege `readBackupFile` |
| `restoreArchive` | `applyOps` |
| `pruneOplog` | `replSetResizeOplog` |
| `pruneTtl` | `compact` |

## TLS and mTLS

Transport security is [rustls](https://github.com/rustls/rustls)-based:

```bash
secantusd-rs \
  --tls-cert-file /etc/letsencrypt/live/db.example.com/fullchain.pem \
  --tls-key-file  /etc/letsencrypt/live/db.example.com/privkey.pem \
  --tls-ca-file   /etc/ssl/client-ca.crt \
  --tls-require-client-cert
```

- `cert-file` + `key-file` wrap every accepted socket in TLS before the
  wire protocol starts. Clients connect with `?tls=true&tlsCAFile=<ca>`.
- `ca-file` turns on client-certificate verification (mTLS). Without
  `--tls-require-client-cert`, a presented cert is verified but clients
  without one are still accepted (useful for staged rollouts); with it,
  certificate-less clients are rejected.
- The verified client-cert subject DN feeds **MONGODB-X509**
  authentication.

The same settings are available in the `[tls]` section of
`secantusd.toml` and as `RustServer(...)` constructor arguments.

## Out of scope

Same non-goals as the Python server: LDAP, Kerberos/GSSAPI, PLAIN,
MONGODB-AWS, MONGODB-OIDC, and keyfile cluster auth (no cluster).
