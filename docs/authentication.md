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

Auth is opt-in. A vanilla `secantusd-py` daemon and the embedded
`SecantusDBServer(...)` accept commands from any connection — same as
running `mongod` without `--auth`. This keeps the test-harness use case
zero-friction.

```bash
secantusd-py --port 27117          # no auth required
```

## Turning auth on

Two equivalent switches:

- CLI flag: `secantusd-py --auth`
- Constructor: `SecantusDBServer(..., require_auth=True)`

With auth on, only the handshake and the SCRAM exchange run on an
unauthenticated connection. Everything else (`find`, `insert`,
`listDatabases`, ...) returns `Unauthorized` (code 13) until the client
completes a `saslStart` / `saslContinue` round-trip.

```bash
secantusd-py --auth --port 27117
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

## MONGODB-X509 — cert-as-username auth

When the daemon is configured with mTLS (`[tls] cert_file` + `key_file`
+ `ca_file` — see [Configuration](configuration.md)), the
**MONGODB-X509** mechanism lets clients authenticate by presenting an
X.509 cert whose subject DN is registered as a username — no SCRAM
challenge / response, no password to manage.

The flow:

1. Client presents its cert during the TLS handshake; SecantusDB
   verifies the cert chains back to the configured `[tls] ca_file`.
2. Server reads the cert's subject DN (RFC 4514 string form,
   e.g. `CN=alice,OU=Eng,O=Acme,C=US`) and stores it on the
   connection.
3. Client sends `{authenticate: 1, mechanism: "MONGODB-X509",
   user: <DN>}` (the legacy command path pymongo / Java / Go / Node
   all use for X509). The server matches the cert DN against the
   user record and the optional `user` field; mismatch is an
   authentication failure.
4. User record must have `MONGODB-X509` in its `mechanisms` array.

### Provisioning an X509 user

```python
# Connect to the bootstrap server (no auth yet, mTLS optional).
admin = MongoClient(server.uri).get_database("$external")

# The username MUST equal the cert's subject DN, exactly as the
# server will see it (RFC 4514 string, most-specific first).
admin.command(
    "createUser",
    "CN=alice,OU=Eng,O=Acme,C=US",
    roles=[{"role": "root", "db": "admin"}],
    mechanisms=["MONGODB-X509"],
    # No pwd — X509 has no password
)
```

Convention: X509 users live on the `$external` virtual auth db (no
real collections), matching mongod. SecantusDB also accepts users
created on `admin` as a fallback so operators with simpler setups
aren't forced into `$external`.

### Connecting

```
mongodb://server:27017/?
  tls=true
  &tlsCAFile=/path/to/ca.pem
  &tlsCertificateKeyFile=/path/to/alice-cert+key.pem
  &authMechanism=MONGODB-X509
  &authSource=$external
```

The `tlsCertificateKeyFile` is the concatenation of the cert chain and
the private key in one PEM file (pymongo's expected format).

### Combining X509 and SCRAM on one user

A user record can carry both mechanisms simultaneously — useful when
migrating from password auth to cert auth, or when some clients
predate cert support. Just include both in `mechanisms`:

```python
admin.command(
    "createUser",
    "CN=alice,OU=Eng,O=Acme,C=US",
    pwd="hunter2",  # required for SCRAM
    roles=[{"role": "root", "db": "admin"}],
    mechanisms=["SCRAM-SHA-256", "MONGODB-X509"],
)
```

The client picks the mechanism per connection — drivers pick the
strongest the server advertises in `saslSupportedMechs`.

## Driver compatibility

Anything that speaks `SCRAM-SHA-256` or `MONGODB-X509` over the
standard MongoDB wire protocol works. Verified end-to-end:

- `pymongo` (sync and `motor`)
- `mongo-go-driver` (Go) — also covers `mongodump` / `mongorestore` /
  `mongosh`, which embed the Go driver
- `mongosh` direct connection
- `mongo-java-driver`, `mongo-node-driver`, `mongo-ruby-driver` (the
  same SCRAM / X509 wire flow they use against real mongod)

`SCRAM-SHA-1` (the pre-MongoDB-4.0 default) is implemented but **not
advertised by default**. Pass `mechanisms=["SCRAM-SHA-1"]` to
`createUser` if a legacy client pins to it.

## What's not here yet

- **LDAP / Kerberos / GSSAPI / MONGODB-AWS / MONGODB-OIDC** —
  alternative external auth mechanisms. SCRAM and X509 cover the
  majority case; the others would each be a separate slice.
- **Internal cluster auth (keyfile / x509)** — only meaningful with
  replica sets / sharding, both out of scope.

These are tracked in `tasks/backlog.md` and will land in follow-up
slices.
