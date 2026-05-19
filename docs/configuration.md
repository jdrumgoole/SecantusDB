# Configuration

The `secantusdb` daemon reads a TOML configuration file for
deployment-shaped knobs that would otherwise have to be passed on
every invocation. CLI flags still work and **override** any value
set in the file, so the file is a per-deployment baseline rather
than a lock-in.

## Precedence

```
SecantusConfig defaults  <  secantusdb.toml  <  explicit CLI flag
```

Concretely:

* If you pass nothing, the daemon behaves exactly as it always has.
* If a `secantusdb.toml` is on the auto-discovery path, its values
  replace the built-in defaults — but each key in the file is
  optional, so anything you don't set keeps the default.
* If you pass `--port 27018` on the command line, that wins over
  whatever `[server] port` says in the file.

## File location

When you don't pass `--config`, the launcher searches:

1. `./secantusdb.toml` — same directory you ran `secantusdb` from
2. `~/.secantus/secantusdb.toml` — per-user
3. `/etc/secantus/secantusdb.toml` — system-wide

First hit wins; if no file is found, all defaults apply. Pass
`--config /path/to/secantusdb.toml` to load a specific file
(disables auto-discovery).

The launcher logs the resolved path at INFO level on startup so
ops staff can confirm which file got picked up:

```
2026-05-18 19:14:02 INFO secantus.cli: loaded config from /etc/secantus/secantusdb.toml
```

## Schema

```toml
[server]
host = "127.0.0.1"
port = 27017
storage_path = "./secantus-data"
log_level = "INFO"        # DEBUG | INFO | WARNING | ERROR
auth = false              # true requires SCRAM-SHA-256 on every non-handshake command
standalone = false        # true drops the single-node replica-set advertisement

[oplog]
retention_seconds = 3600.0      # oldest entry kept; resume tokens older than this fail
max_entries = 100000            # whichever cap (this or retention) hits first wins
noop_heartbeat_seconds = 0.0    # 10.0 to advance cluster time during quiet stretches

[storage]
cache_size = "1G"               # WiredTiger cache; "256M", "1G", "8G"
session_max = 1000              # WT session cap; one per client connection
ttl_sweep_seconds = 60.0        # TTL pruner cadence (mongod default)
sync_on_commit = false          # see "Durability" below

[tls]
cert_file = "/path/to/server.crt"   # PEM cert chain; both keys must be set together
key_file  = "/path/to/server.key"   # matching PEM private key
```

Every key is optional. Unknown keys (or unknown top-level tables)
fail loudly at startup — a typo like `cache_seize` would otherwise
silently leave WT running with the default 1 GB cache, so the
loader rejects it instead.

## Durability — `sync_on_commit`

The most consequential knob is `[storage] sync_on_commit`. It
controls WiredTiger's `transaction_sync` — whether the log record
for a transaction is fsynced to disk *before* the commit returns.

* **`false` (default)** — `transaction_sync=(enabled=false,
  method=fsync)`. Matches mongod's default `writeConcern: {w: 1, j:
  false}`. Log records land in the OS page cache and are flushed on
  the OS's schedule. SIGKILL of the daemon is durable (the OS
  still flushes its cache); true power-loss between commits and the
  next OS flush window can lose data. Chaos runs measure
  ~99.98% persistence under SIGKILL on this setting.

* **`true`** — `transaction_sync=(enabled=true,method=fsync)`. Every
  commit fsyncs the log before returning, so the wire-protocol
  equivalent of `writeConcern: {j: true}` is effectively enforced
  for the whole connection. **Throughput cost is significant** — 1
  to 2 orders of magnitude on small-doc insert workloads, depending
  on whether the underlying disk has battery-backed write cache.

Pick `true` when your application would otherwise be sending
`{j: true}` writes and expecting them to mean something; pick
`false` (the default) when throughput matters more than
power-loss-window durability.

## Sizing — `cache_size`

`cache_size` is a unit-suffixed string passed straight to WT's
`cache_size` config:

| Value | Sized for |
|---|---|
| `"256M"` | Small dev box; working set is a few MB |
| `"1G"` (default) | Single-app local dev / test surrogate |
| `"4G"` | Modest production single-node |
| `"8G"` or more | Larger working sets; tune to fit the hot doc subset |

Bigger cache = more of the working set lives in RAM = better hit
rates = less disk I/O. There's no point sizing the cache larger
than your dataset; WT won't fault more in than it has rows for.

## TLS

`[tls] cert_file` + `[tls] key_file` make the daemon speak TLS. The
loader insists on both-or-neither — half-configured TLS is almost
certainly a deployment mistake, so the server raises a clean
`ValueError` at startup rather than silently falling back to
plaintext.

When TLS is on, every accepted socket goes through Python's
`SSLContext.wrap_socket(server_side=True)` before the connection
thread takes over. Clients connect with
`mongodb://host:port/?tls=true&tlsCAFile=<ca>` — `tls=true` says
"speak TLS"; `tlsCAFile` tells pymongo which CA cert to verify the
server against. Drop `tlsCAFile` if the server uses a publicly
trusted cert (e.g. Let's Encrypt).

Defaults: Python's `PROTOCOL_TLS_SERVER` — TLS 1.2+ only, no
SSLv2/3 fallback, default cipher list. Ciphers and protocol
versions aren't currently configurable from the file; ship a
follow-on slice if your deployment needs a custom suite.

Hot cert rotation isn't supported: the `SSLContext` is built once
at startup and cached. Restart the daemon after renewing the cert
(e.g. wire `certbot renew --post-hook 'systemctl reload
secantusdb'` into your renewal cron — a "reload" is a restart
despite the name).

mTLS (client cert auth, mongod's `MONGODB-X509` mechanism) is not
yet wired up — a follow-on slice. Until then, SCRAM-SHA-256 over
TLS is the auth + confidentiality story.

## Example

A minimal production-shaped config:

```toml
# /etc/secantus/secantusdb.toml
[server]
host = "127.0.0.1"        # behind nginx for TLS termination
port = 27017
storage_path = "/var/lib/secantus/data"
auth = true               # SCRAM users provisioned via createUser

[storage]
cache_size = "4G"
sync_on_commit = true     # j:true durability — the box has a real UPS

[oplog]
retention_seconds = 86400.0  # 24h, generous for resume tokens
noop_heartbeat_seconds = 10.0
```

The full example file ships in the repo at
[`secantusdb.toml.example`](https://github.com/jdrumgoole/SecantusDB/blob/main/secantusdb.toml.example).

## CLI flag equivalents

Every config-file value has a matching CLI flag for one-off
overrides. The mapping:

| TOML key | CLI flag |
|---|---|
| `[server] host` | `--host` |
| `[server] port` | `--port` |
| `[server] storage_path` | `--storage-path` |
| `[server] log_level` | `--log-level` |
| `[server] auth` | `--auth` |
| `[server] standalone` | `--standalone` |
| `[oplog] retention_seconds` | `--oplog-retention-seconds` |
| `[oplog] max_entries` | `--oplog-max-entries` |
| `[oplog] noop_heartbeat_seconds` | `--noop-heartbeat-seconds` |
| `[storage] cache_size` | `--cache-size` |
| `[storage] session_max` | `--session-max` |
| `[storage] sync_on_commit` | `--sync-on-commit` |
| `[tls] cert_file` | `--tls-cert-file` |
| `[tls] key_file` | `--tls-key-file` |

`[storage] ttl_sweep_seconds` is file-only — it's mostly relevant
for tests that need deterministic TTL timing, which already drive
expiry through `prune_ttl(now=...)` directly.
