# Three-droplet DigitalOcean benchmark: SecantusDB vs MongoDB

A benchmark harness that provisions **three DigitalOcean droplets** — one
running a database, two driving load at it over a private VPC — runs a
coordinated measurement, and tears the cluster down.

It measures **SecantusDB and a real MongoDB server back-to-back on the same
droplets**, and reports them side by side with a ratio. Same cores, same
network, same clients, same workload, one after the other: the only variable
left is the database.

It is written in Rust and lives in `crates/secantus-bench`, as two binaries:

| Binary | Runs on | Job |
| --- | --- | --- |
| `do-cluster` | your machine | provisioning, deployment, orchestration, reporting, teardown |
| `do-client` | each droplet | generating load (clients) and sampling resources (server) |

---

## Contents

- [Why three machines](#why-three-machines)
- [Prerequisites](#prerequisites)
- [Quick start](#quick-start)
- [Command reference](#command-reference)
- [Option reference](#option-reference)
- [What gets created](#what-gets-created)
- [Security model](#security-model)
- [How a run works](#how-a-run-works)
- [The workload](#the-workload)
- [Comparing against MongoDB](#comparing-against-mongodb)
- [Reading the report](#reading-the-report)
- [Per-operation latency and writer scaling](#per-operation-latency-and-writer-scaling-perf)
- [At release time](#at-release-time)
- [Cost and teardown](#cost-and-teardown)
- [Benchmarking unreleased code](#benchmarking-unreleased-code)
- [Worked examples](#worked-examples)
- [Troubleshooting](#troubleshooting)
- [Verified](#verified)
- [Limitations](#limitations)

---

## Why three machines

Every other harness under `bench/` shares one host with the server. The load
generator therefore competes with the database for the same cores, page cache
and memory bandwidth, and the "network" between them is loopback. Two things
follow: latency looks better than any real deployment will ever see, and
throughput is capped by whatever the client can drive on the leftover cores.

Splitting the load across two separate machines with real NICs in between
removes both distortions. Two clients rather than one because a single client
droplet tends to saturate before a well-sized server does — and the report
tells you which side ran out of headroom.

The load agent speaks the MongoDB wire protocol directly (through the
project's own `secantus-wire` crate) instead of going through a driver. For a
*server* benchmark that is the right instrument: driver overhead is the usual
reason a client machine saturates first, and removing it moves the bottleneck
back onto the server where it belongs. Measured locally against the same
`secantusd-rs`, the Rust agent drives about **1.6× the operations per second**
of an equivalent pymongo agent at the same worker count.

---

## Prerequisites

1. **A DigitalOcean API token** with read + write scope, from
   <https://cloud.digitalocean.com/account/api/tokens>:

   ```bash
   export DIGITALOCEAN_TOKEN=dop_v1_...
   ```

   `DO_TOKEN`, `DIGITALOCEAN_ACCESS_TOKEN` and `DO_API_TOKEN` are also
   accepted, so an existing shell export works unchanged.

2. **An SSH keypair the harness can use without a passphrase prompt.** Every
   connection runs with `BatchMode=yes`, which cannot prompt, so a
   passphrase-protected key only works if it is already loaded into
   `ssh-agent`. The harness checks this *before* provisioning anything and
   stops with instructions rather than billing you for droplets it cannot
   reach.

   Keys are looked for in this order — `~/.ssh/secantus-bench`,
   `~/.ssh/id_ed25519`, `~/.ssh/id_rsa` — or pass `--ssh-key`. A dedicated
   benchmark key is the recommended setup, because it needs no passphrase and
   grants access to nothing but throwaway droplets:

   ```bash
   ssh-keygen -t ed25519 -N '' -f ~/.ssh/secantus-bench -C secantus-bench
   ```

   The public half is uploaded to your DigitalOcean account if it isn't
   already there, matched on the key body rather than the name.

3. **`curl`, `ssh`, `scp`, `git`, and a Rust toolchain** on your machine.
   That is all — no `doctl`, no Terraform, no cloud SDK. The harness talks to
   the DigitalOcean API through `curl` and to the droplets through `ssh`.

4. **Money.** The default cluster is one `c-4` and two `c-2` dedicated-CPU
   droplets. `invoke do-status` prints the live hourly rate from the API.

---

## Quick start

The whole cycle in one command:

```bash
export DIGITALOCEAN_TOKEN=dop_v1_...
invoke do-bench --duration 120 --workers 16
```

That runs `up` → `deploy` (only what is missing) → `run` → `suspend`, prints a
report, and destroys the droplets. First run takes roughly 10-15 minutes, most
of it provisioning and compiling the load agent.

Step by step, if you would rather keep the cluster between runs:

```bash
invoke do-up                        # create the droplets
invoke do-deploy                    # install the server + build the agent
invoke do-run --duration 300        # measure (repeat as often as you like)
invoke do-status                    # what exists, what it costs
invoke do-suspend --mode destroy    # stop paying
```

Both wrappers shell out to the Rust binary. Use it directly for the full
option surface:

```bash
cargo run --release --manifest-path crates/Cargo.toml -p secantus-bench \
    --bin do-cluster -- --help
```

---

## Command reference

Run these as `invoke do-<name>` or via the binary as `do-cluster <name>`.

### `up` (alias `resume`)

Creates the three droplets, or powers on / restores from snapshot any that
already exist. Idempotent: safe to run repeatedly. It also creates the VPC and
the firewall, waits for every droplet to become active, waits for SSH, and
waits for `cloud-init` to finish.

Fails if a droplet comes up without a private IP — that would mean the VPC
assignment failed and client → server traffic would silently fall back to the
public interface, which is a different (slower, metered) measurement.

### `deploy`

Two halves:

1. **The server binary.** By default downloads the newest
   `secantusdb-v*` GitHub release's Linux x86_64 tarball onto the server
   droplet, verifies its SHA-256 against the published checksum, and installs
   it at `/usr/local/bin/secantusd-rs`. With `--server-build source` it
   instead clones the repo on the droplet and builds `secantusd-rs` from a
   pushed git ref, vendored WiredTiger and all.
2. **The load agent.** Builds `do-client` on the *first* client droplet, then
   pulls the binary back and pushes it to the other client and to the server.
   Compiling once and distributing means every machine runs a byte-identical
   agent and only one pays the build.

   The agent is built from the harness's **own** source — your current `HEAD`,
   or `--agent-ref` — never from the server's release tag. The two are
   independent: the server is what is being measured, the agent is the
   instrument, and the agent crate does not exist at older server tags at all.
   The ref must be pushed, since the droplet clones it from GitHub.

### `run`

The measurement. Restarts the server with a fresh data directory, measures
client → server round-trip time, preloads documents, runs the timed load on
both clients simultaneously, samples server CPU/RSS throughout, collects
everything, and writes the report. Detailed in
[How a run works](#how-a-run-works).

### `all`

`up` → `deploy` → `run` → `suspend` in one go. `--deploy auto` (the default)
probes the droplets and skips deployment when the binaries are already there,
so a snapshot-restored cluster goes straight to measuring. Teardown runs even
if the benchmark fails — a failed run that leaves three droplets billing is a
worse outcome than the failure.

### `perf`

Measures per-operation latency and concurrent-writer scaling on the server
droplet, refreshing `bench/results/latency.json` and
`bench/results/concurrency.json`. Uses only the server droplet. See
[Per-operation latency and writer scaling](#per-operation-latency-and-writer-scaling-perf).

### `suspend` / `destroy`

Tears the cluster down. See [Cost and teardown](#cost-and-teardown).

### `status`

Every droplet, its power state, its public IP, and its **live hourly price
fetched from the API** — never a hardcoded table, which would go stale and
quietly lie. Also lists this cluster's snapshots.

### `ssh <role>`

Opens an interactive shell on `server`, `client-1`, or `client-2`, using the
same key and `known_hosts` the harness uses.

---

## Option reference

### Cluster options (accepted by every command)

| Option | Default | Meaning |
| --- | --- | --- |
| `--prefix NAME` | `secantus-bench` | Name prefix and DigitalOcean tag for every resource |
| `--region SLUG` | `lon1` | DigitalOcean region |
| `--server-size SLUG` | `c-4` | Server droplet plan |
| `--client-size SLUG` | `c-2` | Client droplet plan |
| `--image SLUG` | `ubuntu-24-04-x64` | Base image |
| `--ssh-key PATH` | first of `secantus-bench`, `id_ed25519`, `id_rsa` in `~/.ssh` | Private key; the `.pub` beside it is uploaded |
| `--ssh-cidr CIDR` | your public IP `/32` | Who may reach port 22. A bare IP is accepted and widened to `/32` |

### Provisioning (`up`, `resume`, `all`)

| Option | Default | Meaning |
| --- | --- | --- |
| `--fresh` | off | Ignore existing snapshots and provision from the base image |

### Deployment (`deploy`, `all`)

| Option | Default | Meaning |
| --- | --- | --- |
| `--server-build MODE` | `release` | `release` installs a published binary; `source` builds on the droplet |
| `--server-version TAG` | `latest` | Which `secantusdb-v*` release to install |
| `--server-ref REF` | `HEAD` | Which pushed git ref to build for `--server-build source` |
| `--agent-ref REF` | `HEAD` | Which pushed git ref the **load agent** is built from |

### Engines (`deploy`, `run`, `all`)

| Option | Default | Meaning |
| --- | --- | --- |
| `--engine WHICH` | `both` | `both`, `secantus`, `mongod`, or a comma list |
| `--mongod-version V` | `8.0` | MongoDB major version to install from MongoDB's apt repo |

### Workload (`run`, `all`)

| Option | Default | Meaning |
| --- | --- | --- |
| `--duration SECS` | `120` | Length of the timed phase |
| `--repeat N` | `1` | Measurement passes; engines interleave within each pass and the report gives medians plus spread |
| `--workers N` | `16` | Load threads per client droplet (so 32 across the cluster) |
| `--op-mix SPEC` | `insert=70,find=20,update=10` | Weighted operation mix |
| `--doc-bytes N` | `8192` | Payload bytes per document |
| `--batch-size N` | `1` | Documents per insert (>1 uses a batched insert) |
| `--preload N` | `10000` | Documents loaded per worker before the clock starts |
| `--cache-size SIZE` | half the droplet's RAM | WiredTiger cache, e.g. `4G` |
| `--sync-on-commit` | off | Start the server with `--sync-on-commit` (fsync per commit) |
| `--standalone` | off | Start the server with `--standalone` |
| `--server-flags STR` | none | Extra flags appended to the `secantusd-rs` command line |
| `--keep-data` | off | Do not wipe the server's data directory before the run |
| `--start-delay SECS` | `20` | Lead time before the shared start barrier |
| `--keep-server-running` | off | Leave the server up after the run, for manual poking |
| `--slow-ms MS` | off | Record every operation at or above MS **with its timestamp**, for tail diagnosis |

### Teardown (`suspend`, `destroy`, `all`)

| Option | Default | Meaning |
| --- | --- | --- |
| `--mode MODE` | `destroy` | `destroy`, `snapshot`, or `power-off` |
| `--purge-snapshots` | off | With `--mode destroy`, delete this cluster's snapshots too |
| `--no-suspend` | off | (`all` only) leave the droplets running afterwards |
| `--deploy WHEN` | `auto` | (`all` only) `auto`, `always`, or `never` |

### Environment variables

| Variable | Purpose |
| --- | --- |
| `DIGITALOCEAN_TOKEN` | **Required.** Also `DO_TOKEN`, `DIGITALOCEAN_ACCESS_TOKEN`, `DO_API_TOKEN` |
| `GITHUB_TOKEN` | Optional; only lifts GitHub's 60/hour anonymous rate limit |
| `SECANTUS_BENCH_RESULTS` | Where run artifacts land (default `bench/results/do`) |
| `SECANTUS_BENCH_STATE` | Harness `known_hosts` and scratch files (default `bench/.do-state`) |

---

## What gets created

| Resource | Name | Notes |
| --- | --- | --- |
| Droplet | `secantus-bench-server` | `c-4` by default: 4 dedicated vCPU, 8 GB |
| Droplet | `secantus-bench-client-1` | `c-2`; also the machine that compiles the agent |
| Droplet | `secantus-bench-client-2` | `c-2` |
| VPC | `secantus-bench-vpc` | client → server traffic stays private, free and low-latency |
| Firewall | `secantus-bench-fw` | applied by tag, so it covers droplets while they boot |
| Snapshots | `secantus-bench-<role>-snap` | only with `--mode snapshot` |

Sizes default to **dedicated-CPU** (`c-*`) plans, not shared-CPU (`s-*`). A
shared droplet's steal time varies with whoever else is on the host, which
makes run-to-run comparison meaningless — the one thing a performance harness
must not do. Override with `--server-size` / `--client-size` if you understand
that trade.

Every droplet boots with a cloud-init that installs `chrony` (the two clients
share a wall-clock barrier, so clock discipline matters) and raises
`somaxconn`, the SYN backlog, the ephemeral port range and `fs.file-max`.

---

## Security model

The server runs with authentication **off**. That is what a benchmark wants
and what a public MongoDB port absolutely is not, so the firewall is
load-bearing rather than decorative:

- **Port 27017 is reachable only from droplets carrying the `secantus-bench`
  tag** — by tag, never by address range, never from the internet.
- **The daemon binds its private VPC address**, so it is not listening on the
  public interface at all. Defence in depth behind the firewall.
- **SSH is restricted to your detected public IP.** If detection fails the
  harness stops and asks for `--ssh-cidr` rather than guessing a wider range.
- **The API token never appears on a command line.** It is written to `curl`'s
  stdin config, so it cannot be read out of `ps` on a shared machine.
- **Values passed to remote scripts travel through the environment**, quoted,
  never interpolated into shell text — a URL or git ref containing shell
  metacharacters cannot become a command.

---

## How a run works

1. **Verify.** Every droplet must exist and be `active`.
2. **Restart the server.** A generated systemd unit is installed, the data
   directory is wiped (unless `--keep-data`), and the service is started. The
   unit sets `Restart=no` deliberately: a database that dies mid-benchmark
   must fail the run loudly, because silently restarting would hand back a
   throughput number averaged over an outage. Startup waits for the port to
   accept connections, and surfaces the journal if the process exits first.
3. **Measure the network.** 20 pings from each client to the server's private
   IP; the min/avg/max/mdev goes into the report.
4. **Preload.** Both clients, in parallel, drop their collections, create the
   `n` index, and insert `--preload` documents per worker. This happens before
   the clock starts so preload cost never lands inside the measurement and so
   reads have something to read.
5. **Barrier.** The orchestrator picks a wall-clock instant `--start-delay`
   seconds out and passes it to both clients. Every worker on both droplets
   sleeps until that instant, so the server sees one overlapping load window
   rather than two staggered ones. Each client reports the skew it actually
   achieved, and the report warns if workers missed the barrier.
6. **Load + sample.** Both clients drive the op mix for `--duration` seconds
   while the server droplet samples `/proc/stat` and the server process's RSS
   once a second.
7. **Collect.** Per-client JSON, the server's resource trace, and the server's
   journal are copied back. The service is checked — if it is no longer
   `active`, that becomes a warning that invalidates the run.
8. **Report.** Histograms are merged across every worker on both droplets, the
   summary is rendered, and everything is written to
   `bench/results/do/<run-id>/`.

---

## The workload

Each client droplet runs `do-client`, which spawns `--workers` OS threads,
each with its own connection and its own collection
(`load_<client-id>_w<n>`). Disjoint collections keep `_id` collisions and
per-collection contention out of the measurement.

Three operations:

| Op | What it does |
| --- | --- |
| `insert` | Inserts `--batch-size` documents of `--doc-bytes` payload, with an increasing `n` |
| `find` | Looks up one document by a random `n` (index-backed) |
| `update` | `$inc`s a counter on one document selected by a random `n` |

`--op-mix` weights them; weights are normalised, so `insert=7,find=2,update=1`
and `insert=70,find=20,update=10` are the same thing. Useful presets:

```bash
--op-mix insert=100                      # write path only
--op-mix find=100                        # read path only (needs --preload)
--op-mix insert=70,find=20,update=10     # the default mixed workload
--op-mix insert=50,update=50             # write-heavy with in-place updates
```

Errors are **counted, never retried**. A benchmark that silently retries a
failed operation reports throughput for work the server did not successfully
do. A worker whose connection breaks reconnects once and carries on, with the
failure still counted.

Latency is accumulated into a log-linear histogram (64 sub-buckets per octave,
under 1% bucket error) rather than a sample array. Histograms merge by adding
counts, which is what lets one report combine every worker across both
droplets into a single set of percentiles.

### Diagnosing a tail

A histogram deliberately throws time away, which is the one thing a tail
investigation needs back: whether slow operations arrive *periodically* (a
checkpoint, a prune, a flush) or *at random* (lock contention, eviction) is the
first fork in the diagnosis.

`--slow-ms MS` records every operation at or above the threshold with its
completion timestamp, worker id and operation type, into a `slow_ops` array in
the client's JSON report. It costs nothing when off (the default) and only
touches the slow path when on.

```bash
do-client run --addr HOST:27017 --client-id c1 --workers 8 \
    --duration 90 --slow-ms 5 --out /tmp/result.json
```

Grouping those timestamps into stall events answers the question immediately.
This is how the p99.9 write-tail convoy in `tasks/backlog.md` was found: every
large stall hit *all* workers within the same few milliseconds, at irregular
intervals — which ruled out any fixed-period background task and pointed at a
shared resource instead.

---

## Comparing against MongoDB

`--engine` selects what runs:

| Value | Effect |
| --- | --- |
| `both` (default) | SecantusDB, then MongoDB, back-to-back; prints a comparison |
| `secantus` | SecantusDB only |
| `mongod` | MongoDB only |

MongoDB is installed on the server droplet from **MongoDB's own apt
repository** — not a distro fork, not a container image with its own tuning.
`--mongod-version` picks the major version (default `8.0`) so a rerun months
later still compares against the same thing.

What is held identical, deliberately:

- the same droplets, in the same order, minutes apart;
- the same bind address and port, so the firewall rule and the client command
  line never change;
- the same WiredTiger cache size — one `--cache-size` drives SecantusDB's
  `--cache-size` and mongod's `--wiredTigerCacheSizeGB`;
- the same clients, worker count, document size, and operation mix;
- an empty data directory for each engine, each in its own path.

The engines run **sequentially, never concurrently**: two databases sharing
four cores would measure contention rather than either engine. `--engine both`
runs SecantusDB first, so if the comparison arm fails the primary number has
already been taken.

Ratios are printed in their own senses — throughput above 1.0 is faster,
latency below 1.0 is quicker — because conflating those two directions is how
benchmark tables mislead.

### Repeating a measurement

`--repeat N` runs the whole thing N times. The engines **interleave within
each pass** rather than each running to completion in turn, so thermal drift,
a noisy neighbour, or anything else that changes over the run lands on both
engines roughly equally instead of penalising whichever went last:

```
pass 1: secantusdb, mongod
pass 2: secantusdb, mongod
pass 3: secantusdb, mongod
```

Every figure in the report is then a **median**, not a mean — one pass
disrupted by a checkpoint stall or a busy neighbour should not drag the
headline, and with small N a mean is exactly what an outlier hijacks. A
**spread** column reports `(max - min) / median`, which is the number that says
whether the median is worth quoting at all, and a per-pass table shows the raw
figures in the order they actually ran.

A measured example — three passes at 60 s each:

| engine | ops/s (median) | spread | pass 1 | pass 2 | pass 3 |
| --- | ---: | ---: | ---: | ---: | ---: |
| secantusdb | 7,704 | 3.4% | 7,704 | 7,760 | 7,498 |
| mongod | 15,850 | 1.1% | 15,850 | 16,003 | 15,824 |

Spreads of 1-3% mean the medians are solid. A double-digit spread is the
report telling you not to quote the number until you have found out why.

**Numbers are comparable within a run, not across clusters.** Two different
`c-4` droplets can sit on different host generations; an identical SecantusDB
build measured 6,230 ops/s on one cluster and 8,244 on another. That is
exactly why both engines are measured on the *same* droplets minutes apart,
and why a cross-cluster figure should never be quoted as a change.

---

## Reading the report

Artifacts land in `bench/results/do/<run-id>/`:

| File | Contents |
| --- | --- |
| `comparison.md` | The side-by-side table and ratios (when more than one engine ran) |
| `<engine>-summary.md` | Each engine's own rendered report (`<engine>-pass<N>-…` when repeating) |
| `<engine>-summary.json` | The same data, machine-readable, stable enough to build tooling on |
| `<engine>-client-1.json`, `<engine>-client-2.json` | Each client's raw report, including full histograms |
| `<engine>-server-sample.json` | Per-second server CPU / RSS / free-memory trace |
| `<engine>-journal.log` | The last 200 journal lines from that engine's service |

The summary shows per-client and aggregate throughput, per-operation latency
percentiles, server CPU and peak RSS, and the measured round-trip time.

### Warnings are the important part

A throughput number from a distributed benchmark only means something if the
load landed the way the harness intended. Everything that would make the
headline misleading is detected and printed under the table:

| Warning | What it means | What to do |
| --- | --- | --- |
| *client CPU was N% busy* | The client, not the server, was the limit | Bigger `--client-size`, or fewer `--workers`. Treat the number as a floor |
| *N operations errored* | Throughput counts only successes, so it is inflated | Read `server-journal.log` before believing anything |
| *workers started up to Ns apart* | Some workers missed the shared barrier | Raise `--start-delay` |
| *clients started Ns apart* | The two load windows only partly overlapped | Check clock sync; re-run |
| *server CPU averaged only N%* | The bottleneck is elsewhere | Client capacity, the network, or a serialised path in the server |
| *server service is 'failed'* | The database died during the run | The run is invalid. Diagnose before re-measuring |
| *not every client reported* | The aggregate is partial | Check that client's stderr in the run output |

---

## Per-operation latency and writer scaling (`perf`)

`do-cluster perf` (or `invoke do-perf`) measures the *other* two published
benchmarks on droplet hardware: per-operation latency (`bench.compare_servers`)
and concurrent-writer scaling (`bench.concurrency`).

```bash
invoke do-perf                       # ~1 hour, ~$0.40, droplet destroyed after
invoke do-perf --keep                # leave it running to iterate
invoke do-perf --count 50000 --writers 1,2,4,8,16
```

Unlike the throughput benchmark, this uses **only the server droplet**. Both
harnesses spawn all three engines themselves and drive them over loopback —
that is what makes them per-operation *engine* measurements rather than network
measurements — so a client droplet would contribute nothing but a NIC.

It writes `bench/results/latency.json` and `bench/results/concurrency.json`,
the two files the chart generators read:

```bash
uv run --no-sync python -m bench.latency_chart
uv run --no-sync python -m bench.concurrency_chart --results bench/results/concurrency.json
```

Both rewrite marker-delimited blocks in `docs/benchmark.md`,
`docs/concurrency.md` and the website's `performance.html`. The prose around
each chart is hand-maintained — the generators print the fresh headline ranges
so you can check the sentences against them.

### Why not just run these locally?

Because a developer machine cannot be trusted for them, and the failure is
silent. A run taken immediately after a parallel `cargo`/`cmake` build recorded
*mongod itself* at 2.5x its own baseline (insert 66.7 ms → 185.4 ms); since the
workloads run sequentially while load decays, the ratios were skewed too,
showing a fabricated 0.3x on `find_indexed_range`. Nothing in the output
indicated a problem — the table looked entirely normal.

**`mongod` is the control.** It is measured in the same run and does not change
between releases, so compare it against the previous `latency.json` before
believing anything. If mongod moved, the machine moved.

## Refreshing the published table

`release-benchmark` writes `bench/results/do/<run>/comparison.md`. The published
head-to-head table in `docs/benchmark.md` is generated from it:

```bash
uv run python -m bench.head_to_head_chart          # newest run
uv run python -m bench.head_to_head_chart --check  # exit 1 if the page is stale
```

This used to be a copy-and-paste step, and it went stale twice — once leaving a
post-lz4 droplet section above a pre-lz4 latency table, and once running two
releases behind while the header above it named a different mongod. Both times
nothing failed, because prose with numbers in it rots quietly.
`tests/test_benchmark_table_fresh.py` now runs the `--check` form, so the suite
fails instead.

## At release time

`docs/benchmark.md` publishes a head-to-head comparison against a real
`mongod`. It is prose with numbers in it, so it goes stale **silently** — no
test fails when the engine gets faster, and a release that improves performance
ships a page understating it. That is not hypothetical: the published figures
were measured the day before lz4 replaced zlib as the block compressor, so the
release that nearly doubled write throughput shipped the old numbers.

`invoke release-benchmark` is the release-time re-measurement:

```bash
export DIGITALOCEAN_TOKEN=dop_v1_...
invoke release-benchmark
```

It provisions the cluster, deploys both engines, runs **three interleaved
passes on incompressible payloads**, prints the comparison, and destroys
everything. About 45 minutes and $0.25, most of it deployment.

Then paste the comparison table into `docs/benchmark.md`'s "Over a real
network, against a real MongoDB" section, along with the date and the versions
of both engines.

Two settings are deliberate and should not be relaxed for speed:

- **`--payload random`.** Both engines compress, so the default
  repeated-character payload measures the compressor rather than the engine —
  and it flatters whichever side compresses harder. The published ratio moved
  from 0.46x to 0.27x purely by switching to incompressible data.
- **`--repeat 3`.** A single pass carries no spread, so nothing tells you
  whether the median is worth quoting. Three passes with a sub-5% spread is the
  bar for a published number.

**Cut the Rust binary release first.** `release-benchmark` deploys the newest
published `secantusdb-v*` release by default, so running it before that tag
exists measures the *previous* build. Either tag the binary release first (the
normal order — the binary track is quick) or pass `--server-build source` to
build the current ref on the droplet, which adds about 20 minutes.

---

## Cost and teardown

**DigitalOcean bills a droplet for existing, not for running.** A powered-off
droplet costs exactly as much as a running one. The default teardown is
therefore `destroy`:

| `--mode` | Resume | Ongoing cost | Use when |
| --- | --- | --- | --- |
| `destroy` (default) | full reprovision + deploy | **nothing** | done for now |
| `snapshot` | ~2 min, software intact | snapshot storage only | testing again in weeks |
| `power-off` | ~1 min, software intact | **full price** | testing again within hours |

Measured on a three-droplet cluster: `power-off` took 1m03s to resume with the
public IPs unchanged; `snapshot` took 1m44s to image and destroy, then 2m09s
to restore — with the installed binaries executable on arrival, so `run` works
immediately and `all --deploy auto` skips deployment.

`snapshot` powers each droplet off, images it, destroys it, and prunes the
previous image so snapshots do not accumulate. A later `up` restores droplets
with the server binary and the load agent already installed, and `all` skips
the deploy automatically.

If a run is interrupted, the droplets stay allocated. Any failure prints a
reminder, and `invoke do-status` will show them.

---

## Benchmarking unreleased code

By default `deploy` installs the published Linux binary from the newest
`secantusdb-v*` release — the PGO'd artifact users actually get. To measure a
change that is not released yet:

```bash
git push -u origin my-branch
cargo run --release --manifest-path crates/Cargo.toml -p secantus-bench \
    --bin do-cluster -- deploy --server-build source
```

That clones the repo on the server droplet at your current `HEAD`, builds
vendored WiredTiger through the same path CI uses, and builds `secantusd-rs`
from it. It takes 15-25 minutes and requires the ref to be **pushed** — the
droplet clones from GitHub, so uncommitted work is invisible to it (and
unreproducible as a benchmark anyway). The load agent is built from the same
ref, keeping client and server in step.

---

## Worked examples

**Write-path throughput, batched inserts, long run:**

```bash
invoke do-bench --duration 300 --workers 32 --op-mix insert=100 --batch-size 100
```

**Read-path latency on a warm cache** — preload heavily, then read only:

```bash
invoke do-run --duration 180 --workers 16 --op-mix find=100
```

(with `--preload 50000` via the binary, since the invoke wrapper keeps the
default.)

**Durability cost** — the same workload with and without per-commit fsync:

```bash
invoke do-run --duration 120 --op-mix insert=100
invoke do-run --duration 120 --op-mix insert=100 --sync-on-commit
```

**A bigger server, kept between runs:**

```bash
cargo run --release --manifest-path crates/Cargo.toml -p secantus-bench \
    --bin do-cluster -- all --server-size c-8 --client-size c-4 \
    --workers 32 --duration 300 --no-suspend
```

**Park a cluster you will use again next month:**

```bash
invoke do-suspend --mode snapshot
```

---

## Troubleshooting

**`No DigitalOcean API token`** — export `DIGITALOCEAN_TOKEN`. The token needs
*write* scope; a read-only token fails at droplet creation, not at start-up.

**`Could not detect this machine's public IP`** — your network blocks the
detection endpoints. Pass `--ssh-cidr 203.0.113.7/32` explicitly.

**`ssh to <ip> never came up`** — usually the firewall's SSH rule versus a
changed public IP. Check `invoke do-status`, then re-run `up` (which rewrites
the firewall) or pass the right `--ssh-cidr`.

**`<droplet> has no private IP`** — VPC assignment failed. Destroy and
re-provision; do not measure across the public interface.

**`server never accepted connections`** — the run prints the server journal.
Common causes: a data directory left by an incompatible build (drop
`--keep-data`), or a cache size larger than the droplet's RAM.

**`<ref> is not on any remote branch`** — the droplets clone from GitHub, both
for `--server-build source` and for the load agent. Push the branch first.

**Host key verification failures** — the harness keeps its own `known_hosts`
under `bench/.do-state/`. Deleting that file is safe; it re-learns on the next
connection.

**`is passphrase-protected and is not loaded in ssh-agent`** — the pre-flight
refusing to provision droplets it could not reach. Either `ssh-add` the key, or
create the dedicated passphrase-free key shown in
[Prerequisites](#prerequisites).

**`the droplet rejected the SSH key`** — the key is not in the droplet's
`authorized_keys`. DigitalOcean injects keys only at creation time, so a
droplet made before the key reached your account can never accept it: destroy
and re-provision.

**A run was interrupted** — nothing is cleaned up automatically. Run
`invoke do-status`, then `invoke do-suspend`.

---

## Verified

Run against a live account on 2026-08-21: one `c-4` server and two `c-2`
clients in `lon1`, 16 workers per client, 8 KiB documents, the default
70/20/10 mix, 120 s per engine, 4 GB WiredTiger cache for both.

| engine | version | ops/s | errors | p50 ms | p99 ms | p99.9 ms | server CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| secantusdb | 0.5.3-beta.160 | 8,244 | 0 | 2.33 | 25.07 | 114.03 | 83.2% |
| mongod | v8.0.29 | 17,984 | 0 | 1.49 | 6.94 | 11.58 | 84.5% |

**SecantusDB relative to MongoDB: 0.46x throughput**, 1.57x p50 latency,
3.61x p99, 9.84x p99.9. The gap is widest in the tail.

Both engines saturated the same server (83-85% mean CPU) while the clients sat
at 12-15%, so both numbers are server-bound and the comparison is fair. A
second back-to-back pass reproduced it: 8,212 vs 17,829, the same 0.46x — the
engines individually within 1% of the first pass.

Also verified live: provisioning, VPC and firewall, SSH, cloud-init, the
release-binary deploy, the on-droplet source build (`--server-build source`,
vendored WiredTiger and all), the on-droplet agent build, result collection,
and **all three teardown modes** — `destroy`, `power-off` (resume with IPs and
software intact), and `snapshot` (image, destroy, then restore with the
binaries still executable), including `--purge-snapshots`.

---

## Limitations

- **A single pass reports no spread.** `--repeat 1` (the default) is one
  measurement per engine, so nothing tells you how stable it was. Use
  `--repeat 3` or more before quoting a number that matters.
- Single server droplet only — SecantusDB is single-node by design, so there
  is nothing to shard or replicate across.
- The agent measures `insert` / `find` / `update`. Aggregation, change
  streams, and transactions would each need their own operation type.
- MongoDB is run as a standalone `mongod`, which is the like-for-like
  comparison: SecantusDB is single-node by design. A real replica set would
  pay replication costs SecantusDB never pays, which would flatter SecantusDB
  rather than inform.
- The agent is not a real driver, so it does not measure driver-side cost.
  That is deliberate (see [Why three machines](#why-three-machines)), but it
  means these numbers are a *server* ceiling, not an application forecast.
- No cross-run comparison or charting yet; `summary.json` is stable enough to
  build one on.
