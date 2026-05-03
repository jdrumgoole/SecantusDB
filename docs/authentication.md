# Authentication

SecantusDB implements **SCRAM-SHA-256**, MongoDB's default authentication
mechanism since 4.0, end-to-end on the wire. The same `MongoClient(uri,
username=, password=)` calls you'd point at a real `mongod` work against
SecantusDB unchanged.

This is wire-protocol authentication only — `saslStart` / `saslContinue`
plus the `createUser` / `dropUser` / `usersInfo` admin commands and the
per-connection state machine. **Authorization (RBAC) is not implemented
yet**: an authenticated principal is treated as fully privileged. See
[Compatibility](compatibility.md) and `tasks/backlog.md` for the
remaining gaps.

## Off by default

Auth is opt-in. A vanilla `secantusdb` daemon and the embedded
`SecantusDBServer(...)` accept commands from any connection — same as
running `mongod` without `--auth`. This keeps the test-harness use case
zero-friction.

```bash
secantusdb --port 27117          # no auth required
```

## Turning auth on

Two equivalent switches:

- CLI flag: `secantusdb --auth`
- Constructor: `SecantusDBServer(..., require_auth=True)`

With auth on, only the handshake and the SCRAM exchange run on an
unauthenticated connection. Everything else (`find`, `insert`,
`listDatabases`, ...) returns `Unauthorized` (code 13) until the client
completes a `saslStart` / `saslContinue` round-trip.

```bash
secantusdb --auth --port 27117
```

## Provisioning users

Use `createUser` from any connected client. The plaintext password is
hashed (PBKDF2-HMAC-SHA-256, 15000 iterations) and discarded; only the
SCRAM `storedKey` and `serverKey` are persisted.

```python
from pymongo import MongoClient

# Connect once (no auth required to call createUser when --auth is off).
admin = MongoClient("mongodb://127.0.0.1:27117/").admin
admin.command(
    {
        "createUser": "alice",
        "pwd": "hunter2",
        "roles": [],            # roles accepted but not enforced (RBAC TBD)
    }
)
```

Then connect with credentials. Pymongo handles the SCRAM round-trip:

```python
client = MongoClient(
    "mongodb://127.0.0.1:27117/",
    username="alice",
    password="hunter2",
    authSource="admin",
    authMechanism="SCRAM-SHA-256",
)
client["mydb"]["users"].insert_one({"_id": 1, "name": "Alice"})
```

Or as a single URI:

```
mongodb://alice:hunter2@127.0.0.1:27117/?authSource=admin&authMechanism=SCRAM-SHA-256
```

`mongosh`, `mongodump`, and other tooling work the same way.

## Bootstrap pattern

The chicken-and-egg problem: with `--auth` on you can't `createUser`
without already being authenticated. There are two ways out:

1. **Provision first, lock down second** — start without `--auth`, run
   `createUser`, restart with `--auth`. The on-disk store keeps the
   user record across restarts.
2. **Pre-seed in code** — for embedded use, reach into
   `server.storage.add_user(...)` directly to insert the credential
   record before clients connect. The shape mirrors mongod's
   `admin.system.users[]` documents:

```python
from secantus import SecantusDBServer
from secantus.auth import SCRAM_SHA_256, derive_credentials

srv = SecantusDBServer(port=0, storage_path=":memory:", require_auth=True)
srv.start()
creds = derive_credentials("secret")
srv.storage.add_user(
    "admin",
    "root",
    {
        "_id": "admin.root",
        "user": "root",
        "db": "admin",
        "credentials": creds.to_doc(),
        "roles": [{"role": "root", "db": "admin"}],
        "mechanisms": [SCRAM_SHA_256],
    },
)
```

## Inspecting state

`usersInfo` returns provisioned users (without credentials by default).
`connectionStatus` returns who's authenticated on the current connection.

```python
admin.command({"usersInfo": 1})                                # all users in this db
admin.command({"usersInfo": "alice"})                          # one
admin.command({"usersInfo": "alice", "showCredentials": True}) # include hashes
admin.command("connectionStatus")                              # auth state
```

## Driver compatibility

Anything that speaks `SCRAM-SHA-256` over the standard MongoDB wire
protocol works. Verified end-to-end:

- `pymongo` (sync and `motor`)
- `mongo-go-driver` (Go) — also covers `mongodump` / `mongorestore` /
  `mongosh`, which embed the Go driver
- `mongosh` direct connection

`SCRAM-SHA-1` (the pre-MongoDB-4.0 default) is **not** advertised.
Modern drivers default to SHA-256; legacy clients pinned to SHA-1 will
need to be updated.

## What's not here yet

- **Role-based access control** — `roles` arrays are stored but never
  consulted. Any authenticated user can run any command.
- **`updateUser`** — drop and recreate to rotate a password.
- **`grantRolesToUser` / `revokeRolesFromUser` / `createRole` / etc.**
- **x509 / LDAP / Kerberos / GSSAPI / MONGODB-AWS / MONGODB-OIDC** —
  only SCRAM-SHA-256 is implemented.
- **TLS** — see [Compatibility](compatibility.md). SCRAM travels in
  plaintext; suitable for a trusted network only.
- **SASLprep (RFC 4013)** password normalisation — ASCII passwords are
  fine; non-ASCII may diverge from a normalising client.

These are tracked in `tasks/backlog.md` and will land in follow-up
slices.
