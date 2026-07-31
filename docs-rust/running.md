# Running the daemon

```bash
secantusd-rs --host 127.0.0.1 --port 27017 --storage-path ./secantus-data
```

The daemon prints `secantusd-rs listening on <host>:<port>` once the accept
loop is up (launchers can parse that line), runs until SIGINT/SIGTERM, and
shuts WiredTiger down cleanly on exit. `--port 0` binds an OS-assigned
port. Bad arguments exit 2; `--help` / `--version` exit 0.

## CLI flags

Both `--flag value` and `--flag=value` spellings work.

| Flag | Meaning | Default |
| --- | --- | --- |
| `--config PATH` | Explicit `secantusd.toml` (see below) | auto-discovered |
| `--host HOST` | Bind address | `127.0.0.1` |
| `--port N` | TCP port (`0` = OS-assigned) | `27017` |
| `--storage-path PATH` | WiredTiger home directory (created if absent) | `./secantus-data` |
| `--log-level LEVEL` | `DEBUG` / `INFO` / `WARNING` / `ERROR` | `INFO` |
| `--cache-size SIZE` | WiredTiger cache, unit-suffixed (`256M`, `1G`, `8G`) | `1G` |
| `--session-max N` | Max concurrent WiredTiger sessions (≈ client connections) | `1000` |
| `--sync-on-commit` | fsync every commit (closes the `j: true` gap; large throughput cost) | off |
| `--oplog-async` | Persist oplog entries via a background drainer pool instead of inside each write's commit (higher write throughput; change-stream events surface once drained) | off, or `SECANTUS_OPLOG_ASYNC` |
| `--oplog-nonlogged` | Create oplog tables `log=(enabled=false)` — checkpoint-durable only; a crash can lose the oplog tail | off, or `SECANTUS_OPLOG_NONLOGGED` |
| `--data-nonlogged` | mongod's split: WAL-log only the oplog; data tables recover by oplog replay from the last stable checkpoint (see [Recovery](recovery.md)) | off, or `SECANTUS_DATA_NONLOGGED` |
| `--checkpoint-seconds N` | Stable-checkpoint cadence for `--data-nonlogged` | `60`, or `SECANTUS_CHECKPOINT_SECONDS` |
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

## Configuration file

Both MongoDB-wire daemons (`secantusd-py` and `secantusd-rs`) read the same
`secantusd.toml`, discovered in order:

1. `./secantusd.toml` (per-checkout / per-cwd)
2. `~/.secantus/secantusd.toml` (per-user)
3. `/etc/secantus/secantusd.toml` (system-wide)

or passed explicitly with `--config`. Every key is optional; explicit CLI
flags override the file. The sections mirror the flags (a handful of keys
are Rust-daemon-only — the commented `[storage]` modes below — and the
Python daemon rejects them, so keep them out of a `secantusd.toml` shared
with `secantusd-py`):

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
# Rust-daemon-only storage modes (secantusd-py rejects these keys):
# oplog_async = true
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
