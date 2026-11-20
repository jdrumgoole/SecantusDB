# Running the daemon

```bash
secantusd-mdb --host 127.0.0.1 --port 27017 --storage-path ./secantus-data
```

The daemon prints `secantusd-mdb listening on <host>:<port>` once the accept
loop is up (launchers can parse that line), runs until SIGINT/SIGTERM, and
shuts WiredTiger down cleanly on exit. `--port 0` binds an OS-assigned
port. Bad arguments exit 2; `--help` / `--version` exit 0.

## CLI flags

Both `--flag value` and `--flag=value` spellings work.

| Flag | Meaning | Default |
| --- | --- | --- |
| `--config PATH` | Explicit `secantusd.toml` (see below) | auto-discovered |
| `--host HOST` | Bind address; also `--bind_ip`, mongod's spelling (one address) | `127.0.0.1` |
| `--port N` | TCP port (`0` = OS-assigned) | `27017` |
| `--storage-path PATH` | WiredTiger home directory (created if absent); also `--dbpath`, mongod's spelling | `./secantus-data` |
| `--log-level LEVEL` | `DEBUG` / `INFO` / `WARNING` / `ERROR` | `INFO` |
| `--cache-size SIZE` | WiredTiger cache, unit-suffixed (`256M`, `1G`, `8G`) | `4G` |
| `--session-max N` | Max concurrent WiredTiger sessions (≈ client connections) | `1000` |
| `--sync-on-commit` | fsync every commit (closes the `j: true` gap; large throughput cost) | off |
| `--oplog-nonlogged` | Create oplog tables `log=(enabled=false)` — checkpoint-durable only; a crash can lose the oplog tail. Only with `--data-logged`: on the default unlogged data tables the data is recovered from the oplog, so the server refuses it there (a crash would lose every write since the last checkpoint) | off, or `SECANTUS_OPLOG_NONLOGGED` |
| `--data-nonlogged` | mongod's split: WAL-log only the oplog; data tables recover by oplog replay from the last stable checkpoint (see [Recovery](recovery.md)), and keep the history a snapshot read at a past time (`atClusterTime`) reads. Decided when a store is created; an existing store keeps its mode | on |
| `--data-logged` | WAL-log the data tables too. No history: a snapshot read at a past time is refused (246) | off |
| `--checkpoint-seconds N` | Seconds between checkpoints. A checkpoint is what trims the log, and under `--data-nonlogged` what makes the data durable | `60`, or `SECANTUS_CHECKPOINT_SECONDS` |
| `--auth` | Require SCRAM authentication on every non-handshake command | off |
| `--standalone` | Advertise a plain standalone `hello` instead of the single-node replica-set persona (disables change-stream topology, matters for drivers that gate on `isReplicaSet`) | off |
| `--noop-heartbeat-seconds N` | Periodic oplog no-op so cluster time advances while idle (mongod default is 10s) | `0` (disabled) |
| `--oplog-retention-seconds N` | Oplog retention window | `3600` |
| `--oplog-max-entries N` | Oplog entry cap (whichever cap hits first prunes oldest) | `100000` |
| `--oplog-archive-dir PATH` | Archive pruned oplog rows for [point-in-time recovery](recovery.md) beyond the live window | off |
| `--tls-cert-file PATH` | Server certificate (PEM); pair with `--tls-key-file` | off |
| `--tls-key-file PATH` | Server private key (PEM) | off |
| `--tls-ca-file PATH` | Client-cert CA bundle — turns on mTLS verification | off |
| `--tls-require-client-cert` | Reject clients that present no certificate | off |

TLS pairing rules are enforced: setting one of `cert-file` / `key-file`
without the other is a startup error, and the client-cert flags are only
meaningful with server TLS configured. See [Security](security.md).

## Running it as a service

`secantusd-mdb service install` runs the server in the background for your
user, started at login, under the operating system's own service manager:
a launchd LaunchAgent on macOS, a systemd user unit on Linux. That is how
mongod's packages run mongod too; the server itself stays a foreground
process, and the service manager stops it with SIGTERM, so WiredTiger
closes cleanly.

```bash
secantusd-mdb service install --port 27017      # start it, now and at every login
secantusd-mdb service status                     # running? listening where?
secantusd-mdb service uninstall                  # stop it; keeps the data
```

`install` copies the binary it was run as to `~/.secantus/bin`, so the
service keeps working after the virtualenv or download it came from is
gone, writes the service file, starts the service and waits until the
server says it is listening. The options after `install` are the server's
own (any flag in the table above); without `--storage-path` or `--dbpath`
the store is `~/.secantus/data`, and `~/.secantus/secantusd.toml` is read
as usual. Running `install` again replaces the service (a new binary, new
options) and keeps the data. `--dry-run` prints the service file and the
commands without doing anything.

| What | Where |
| --- | --- |
| The binary the service runs | `~/.secantus/bin/secantusd-mdb` |
| The store (default) | `~/.secantus/data` |
| The server's output | `~/.secantus/secantusd-mdb.log` |
| The service file (macOS) | `~/Library/LaunchAgents/com.secantusdb.secantusd-mdb.plist` |
| The service file (Linux) | `~/.config/systemd/user/secantusd-mdb.service` |

As with mongod's own unit and Homebrew's plist, the service does **not**
restart a server that exits: a database that stopped on an error stays
down where its log can be read. The service raises the open-file limit to
64000 (mongod's packaged value; launchd's default is 256) and gives the
server time to close cleanly before it is killed: 60 seconds under launchd
(the most launchd allows; its default is 5) and 90 under systemd. `status` exits 0 when the service is running and
3 when it is not. On Linux a user service stops at logout unless lingering
is on (`loginctl enable-linger $USER`). Windows is not supported yet.

## Slow operations

An operation that runs for 100 ms or longer writes one line to the log, at
`INFO`, as `mongod` does:

```text
[2026-10-09T19:10:59Z INFO  secantus_server] Slow query {"type":"command","ns":"bench.stall","command":"insert","ctx":"conn3","ninserted":100,"workingMillis":1717,"durationMillis":1717}
```

The same lines are in the `getLog` buffer. `workingMillis` leaves out time an
awaitData `getMore` spent waiting for new data, so an idle change stream logs
nothing. A failed operation carries `"ok":0` with `errCode` and `errName`.

The threshold is the `profile` command's `slowms`, and `sampleRate` thins the
lines. Both are server-wide and last until the server stops:

```python
client.admin.command("profile", 0, slowms=20)   # log anything over 20 ms
```

The line says which operation was slow and for how long. It does not say where
in the storage engine the time went, which `mongod`'s line does.

## Configuration file

The daemon (`secantusd-mdb`) reads a
`secantusd.toml`, discovered in order:

1. `./secantusd.toml` (per-checkout / per-cwd)
2. `~/.secantus/secantusd.toml` (per-user)
3. `/etc/secantus/secantusd.toml` (system-wide)

or passed explicitly with `--config`. Every key is optional; explicit CLI
flags override the file. The sections mirror the flags:

```toml
[server]
host = "127.0.0.1"
port = 27017
storage_path = "./secantus-data"
log_level = "INFO"
auth = false
standalone = false

[oplog]
retention_seconds = 3600.0
max_entries = 100000
noop_heartbeat_seconds = 0.0
# archive_dir = "/var/lib/secantus/pitr-archive"

[storage]
cache_size = "1G"
session_max = 1000
sync_on_commit = false
# Storage modes:
# oplog_nonlogged = true
# data_nonlogged = true
# checkpoint_seconds = 60

# [tls]
# cert_file = "/etc/letsencrypt/live/db.example.com/fullchain.pem"
# key_file  = "/etc/letsencrypt/live/db.example.com/privkey.pem"
# ca_file   = "/etc/ssl/client-ca.crt"
# require_client_cert = false
```

The annotated example ships as `secantusd.toml.example` in the repository
root. (The legacy `secantusdb.toml` filename is still auto-discovered at
each location.)

## Topology persona

By default the server advertises itself in `hello` as the primary of a
single-node `secantus` replica set — a deliberate fiction that makes
drivers' topology machinery accept change streams. There are no other
members and no elections. `--standalone` switches to a plain standalone
`hello` for tests and drivers that behave differently against replica
sets (the Java driver's `getSecondary()` helper, for example, spins
forever against a replica-set persona with no secondary).
