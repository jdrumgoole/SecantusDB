# Running SecantusDB in production

## Should you?

SecantusDB started life as a **single-node test surrogate** for the
MongoDB wire protocol — fast, embeddable, ephemeral, "don't stand up
a real `mongod` just to run pytest." That's still the primary
audience. The storage engine underneath (WiredTiger, the same engine
real `mongod` uses) is genuinely production-grade, and the supported
wire-protocol surface is conformant enough that production drivers
talk to it cleanly. So the comparison most people reach for —
"SecantusDB vs `mongod` in prod" — isn't the useful one. The useful
comparison is **SecantusDB vs single-node Postgres** for a small or
medium application that wants document-shaped storage on one box.

Single-node Postgres is a perfectly normal production setup. Millions
of small / internal apps run on one Postgres box without a cluster.
SecantusDB *can* play the same role for a document workload. This
page is honest about where the parallel holds and where it doesn't,
and then walks through a concrete deployment shape.

If your workload demands high availability, sub-second failover,
streaming logical replication, multi-region writes, or any kind of
sharding — **use real MongoDB or a managed cluster**. SecantusDB
doesn't pretend to compete in that space.

## Where SecantusDB stands today

| Concern | Postgres single-node | SecantusDB single-node |
|---|---|---|
| Storage engine maturity | PG MVCC, 25+ years | WiredTiger, used in `mongod` prod for a decade+ |
| Crash recovery from journal | WAL replay on start | WT log replay on `wiredtiger_open` |
| Per-commit durability | `synchronous_commit=on` default | `sync_on_commit=true` opt-in (see [Configuration](configuration.md)) |
| Full snapshot backup | `pg_basebackup` | `secantusAdmin.backupArchive` (`.tar.gz`) |
| Restore | swap dbpath + start | `secantusdb-restore-archive` → start fresh server |
| Point-in-time recovery | WAL archiving + `recovery_target_time` | not supported |
| Native TLS | `ssl=on` | server-side TLS via `[tls] cert_file` / `key_file` |
| Auth methods | md5 / scram / cert / peer / ldap / gss / pam | SCRAM-SHA-256 only |
| Constraints / triggers / FKs / views | rich | none (document store) |
| Replication | streaming + logical | none (single-node by design) |
| Indexing | B-tree / partial / multikey / GIN / GIST / BRIN | B-tree / compound / unique / partial / multikey / sparse / TTL / 2d / 2dsphere |
| Profiling | `log_min_duration_statement` + `pg_stat_*` | `profile` command + admin UI |
| Maturity | production-hardened | beta (b20 at time of writing) |

The headline gaps you must accept up front are **no PITR** (your RPO
is "however often your snapshot cron fires"), **no replication /
failover** (a process crash without a hot standby is downtime), and
**beta maturity** (the project has not yet been through a public
production incident — the existence of unknown sharp edges has not
been ruled out). Native TLS used to be on this list; it now isn't —
`[tls] cert_file` / `key_file` in `secantusdb.toml` wraps every
accepted socket in TLS before the wire protocol starts, so a reverse
proxy is no longer required to put SecantusDB behind a stable
hostname.

If those are acceptable trade-offs for your application, the rest of
this page describes a workable single-node deployment.

## Deployment shape

### `systemd` service unit

A minimal unit that survives crashes, runs as an unprivileged user,
and lets the daemon bind on the public interface (TLS terminates
in-process — see [TLS](#tls) below):

```ini
# /etc/systemd/system/secantusdb.service
[Unit]
Description=SecantusDB
After=network.target

[Service]
Type=simple
User=secantus
Group=secantus
ExecStart=/usr/local/bin/secantusdb
Restart=on-failure
RestartSec=2
LimitNOFILE=65535
ReadWritePaths=/var/lib/secantus
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

Notice the absence of CLI flags on `ExecStart`. Configuration lives
in `/etc/secantus/secantusdb.toml` instead so ops can edit a file
rather than re-deploy the unit.

### Configuration file

[Configuration](configuration.md) is the canonical reference for the
TOML schema; the production-shaped version of that file looks
roughly like this:

```toml
# /etc/secantus/secantusdb.toml
[server]
host = "0.0.0.0"                     # public bind; TLS terminates inside the daemon
port = 27017
storage_path = "/var/lib/secantus/data"
log_level = "INFO"
auth = true                          # SCRAM-SHA-256 enforced on every command

[tls]
cert_file = "/etc/letsencrypt/live/db.example.com/fullchain.pem"
key_file  = "/etc/letsencrypt/live/db.example.com/privkey.pem"

[storage]
cache_size = "4G"                    # size to fit the hot doc subset
sync_on_commit = true                # per-commit fsync (closes the j:true gap)

[oplog]
retention_seconds = 86400.0          # 24h, generous for change-stream resume tokens
noop_heartbeat_seconds = 10.0        # mongod's default cadence
```

`sync_on_commit = true` is the one knob most people get wrong by
omission. Without it, SecantusDB matches mongod's default
`writeConcern: {w:1, j:false}` — durable across SIGKILL of the
process, vulnerable to power-loss between commits and the next OS
flush. With it, every commit fsyncs the log before returning. The
throughput cost is significant (1-2 orders of magnitude on small-doc
inserts, depending on whether your disk has a battery-backed cache),
which is why the default is off; turn it on for any deployment where
a power loss losing the last few seconds of writes would be
unacceptable.

If you'd rather pay the fsync cost only on writes that ask for it
(rather than the whole daemon), leave `sync_on_commit = false` and
have your application send `writeConcern: {w: 1, j: true}` on the
specific writes that need durability. SecantusDB routes the
per-write flag through to a `commit_transaction("sync=on")` so the
j:true subset pays the fsync cost while everything else stays
fast. Most apps want one of the two extremes (everything durable
or everything fast); the per-write route exists for mixed-criticality
workloads.

### Provisioning authentication

SCRAM users must exist before `--auth` / `[server] auth = true` is
enforced, or you'll lock yourself out. The bootstrap sequence:

```bash
# 1. Start once without auth.
secantusdb --storage-path /var/lib/secantus/data --no-auth

# 2. Create an admin in a separate shell.
mongosh mongodb://127.0.0.1:27017/ --eval '
  db.getSiblingDB("admin").createUser({
    user: "admin",
    pwd:  "<strong password>",
    roles: [{role: "root", db: "admin"}],
  })
'

# 3. Stop the daemon, then start it via systemd with auth on.
sudo systemctl start secantusdb
```

From there, application users get provisioned over the wire with
`db.createUser({...})` using the admin credentials. See
[Authentication](authentication.md) for the full mechanism, role
catalogue, and gotchas.

### TLS

Native TLS lives behind the `[tls]` table — when `cert_file` and
`key_file` are both set, the daemon wraps every accepted socket
with TLS before the wire protocol starts. No reverse proxy
required.

```toml
[tls]
cert_file = "/etc/letsencrypt/live/db.example.com/fullchain.pem"
key_file  = "/etc/letsencrypt/live/db.example.com/privkey.pem"
```

Clients connect with `?tls=true`:

```
mongodb://admin:<pwd>@db.example.com:27017/?tls=true
```

Add `&tlsCAFile=/path/to/ca.pem` if the cert is signed by a CA the
client doesn't already trust. Use Let's Encrypt for a public host
or your internal CA for an intranet deployment; the daemon doesn't
care which issued the cert — Python's `ssl` module loads any
PEM-format chain.

Cert rotation is hot-swap-friendly *only* across a restart — the
SSLContext is built once at startup and cached for the lifetime
of the daemon. Bake `certbot renew --post-hook 'systemctl reload
secantusdb'` (a full restart, despite the name) into your renewal
cron so renewed certs take effect.

For deployments that want stronger client-identity guarantees, add
`[tls] ca_file` + `[tls] require_client_cert = true` for mTLS — the
server will then refuse any client that doesn't present a cert
signed by your configured CA. That's a transport-layer coarse
gate ("you're someone we approved of"); SCRAM-SHA-256 still
identifies the specific user on top. mongod's MONGODB-X509 auth
mechanism (cert subject DN as the username, no SCRAM step) is a
separate follow-on slice.

### Backups

Two backup paths ship; pick one and stick with it.

* **Native checkpoint archive** — `secantusAdmin.backupArchive` over
  the wire produces a `.tar.gz` of the WT directory after forcing a
  checkpoint. Fast (no per-doc BSON re-encoding), atomic (consistent
  snapshot of all collections), but SecantusDB-specific (no other
  database can read it).
* **`mongodump`** — slower (walks every collection through the wire),
  produces a portable BSON dump that any `mongod` can ingest. Useful
  when you want to keep the door open to migrate off SecantusDB.

For the native path, a simple hourly cron + off-host sync:

```bash
# /etc/cron.d/secantusdb-backup
0 * * * * secantus mongosh mongodb://admin:<pwd>@127.0.0.1:27017/ \
    --quiet --eval 'db.adminCommand({"secantusAdmin.backupArchive": 1, \
    outputPath: "/var/lib/secantus/backups/archive-$(date -u +\%FT\%H).tar.gz"})'
5 * * * * secantus rsync -a --remove-source-files \
    /var/lib/secantus/backups/ backup-host:/srv/secantus-backups/
```

**Restore lives in the offline CLI**:

```bash
secantusdb-restore-archive \
    --archive /srv/secantus-backups/archive-2026-05-18T03.tar.gz \
    --target-dir /var/lib/secantus/data-restored
sudo systemctl stop secantusdb
sudo mv /var/lib/secantus/data /var/lib/secantus/data-old
sudo mv /var/lib/secantus/data-restored /var/lib/secantus/data
sudo systemctl start secantusdb
```

This is also reachable from the admin UI's `/backup` page (per-row
**Extract** button on `.tar.gz` rows).

**Test your restore.** A backup you have never restored is not a
backup. Schedule a quarterly drill: pull the latest archive, extract
into a sandbox directory, start a fresh SecantusDB pointed at it,
confirm the dataset is intact. Catch corruption before you need it
to work.

### Monitoring

The admin UI (`secantusdb-admin --no-window --uri mongodb://...`)
runs headless behind the same TLS proxy and gives you a dashboard
with insert / query / update / delete rate sparklines, the live
connection list, slow-query profiler, and oplog window inspector.
See [Admin web UI](admin.md) for the full page tour.

For Prometheus / Datadog, scrape `serverStatus` over the wire on a
1-second interval and pull the counters you care about:

```python
import time
from pymongo import MongoClient

client = MongoClient("mongodb://monitor:<pwd>@127.0.0.1:27017/")
while True:
    s = client.admin.command("serverStatus")
    publish_metric("secantus.connections.current", s["connections"]["current"])
    publish_metric("secantus.opcounters.insert", s["opcounters"]["insert"])
    publish_metric("secantus.opcounters.query", s["opcounters"]["query"])
    publish_metric("secantus.opcounters.update", s["opcounters"]["update"])
    publish_metric("secantus.opcounters.delete", s["opcounters"]["delete"])
    time.sleep(1)
```

Alert on connections-per-second spikes, an opcounter going flat (no
traffic = something's wrong upstream), and disk usage growing
faster than your retention pruner can keep up with.

### Capacity sizing

`cache_size` is the knob that matters most. WT caches recently-read
pages in RAM; the bigger the cache, the more of your working set
stays hot, the less your disk gets hit. There's no point sizing the
cache larger than your dataset — WT won't fault more in than there
is.

| Working set | `cache_size` |
|---|---|
| ~100 MB or less | `"256M"` |
| Single small app, ~1 GB | `"1G"` (default) |
| Modest internal app, a few GB | `"4G"` |
| Larger working sets | `"8G"`+, up to ~70% of host RAM |

`session_max = 1000` (the default) gives generous headroom for
concurrent client connections plus change-stream tailers. If you're
running into "out of sessions" errors, bump it; the WT hard cap is
significantly higher than 1000.

### Disaster recovery

With no replication and no PITR, your DR story is "restore the most
recent off-host snapshot." That means:

* **RPO**: the gap between backups. With hourly snapshots you can
  lose up to an hour of writes; with daily snapshots, up to a day.
  Pick the cadence that matches what your application can tolerate.
* **RTO**: time to spin up a new host, extract the latest snapshot,
  flip DNS / load balancer. Minutes-not-seconds at best — be honest
  about this in your SLA conversations.
* **Hot standby**: there isn't a streaming replication story. You
  *can* run a second SecantusDB pre-warmed from the latest snapshot
  on a standby box and switch DNS to it on failure, but the
  switchover loses whatever writes happened between the last
  snapshot and the failure.

If any of those numbers don't fit your business requirements, this
is the signal that SecantusDB's "single-node by design" stance has
caught up with you, and the right answer is a real MongoDB
deployment (managed or self-hosted) rather than a workaround.

## Summary

SecantusDB *can* run as a single-node production document store for
small or internal-scale applications, with the same shape of
deployment a single-node Postgres would take. The deployment story
is straightforward: a `systemd` unit, a `secantusdb.toml` with
`sync_on_commit = true` and a right-sized cache, SCRAM auth,
periodic native backups synced off-host, a TLS-terminating reverse
proxy, and a quarterly restore drill.

The features that make SecantusDB worth choosing — embeddability,
WiredTiger-class storage, the same wire protocol your apps already
speak to mongod — are the same ones it shares with the project's
test-surrogate origin. The features that make production *scary* —
replication, PITR, mature TLS / auth integrations, decades of
incident-tested edge-case handling — are the ones it doesn't have
yet. Treat the comparison table above as the truth-in-advertising
disclosure, and make the call from there.
