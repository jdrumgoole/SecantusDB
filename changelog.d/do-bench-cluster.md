### A three-machine benchmark harness for the Rust server

Every performance harness in this project has shared one host with the server
under test. That means the load generator competes with the database for the
same cores and page cache, and the "network" between them is loopback —
latency that no real deployment will ever see, and a throughput ceiling set by
whatever the client can drive on the leftover cores.

`crates/secantus-bench` measures the other shape. It provisions three
DigitalOcean droplets — one running the standalone `secantusd-rs` daemon, two
driving load at it across a private VPC — runs a coordinated benchmark with
both clients loading the server over the same wall-clock window, collects the
per-client results plus the server's own CPU and memory trace, and tears the
cluster down afterwards. One command does the lot: `invoke do-bench`.

The load agent speaks the MongoDB wire protocol directly through the project's
`secantus-wire` crate rather than through a driver. For a server benchmark
that is the right instrument: driver overhead is the usual reason a client
machine saturates before the database does. Measured against the same local
`secantusd-rs`, it drives about 1.6x the operations per second of an
equivalent pymongo agent at the same worker count.

The report is built to say when it should not be believed. It warns when a
client saturated its own CPU (the client, not the server, was the limit), when
operations errored (throughput counts only successes), when the two clients'
load windows failed to overlap, when the server sat idle enough that the
bottleneck must be elsewhere, and when the server process died during the run
— which invalidates the run outright.

Verified against a live DigitalOcean account: one `c-4` server and two `c-2`
clients in `lon1` sustained **6,230 operations per second with zero errors**
over 120 seconds, with the server at 82.9% mean CPU and the clients at 5-7% —
the shape a valid server benchmark should have, since the instrument was
nowhere near its own limit. Two consecutive runs agreed to within 0.16%.

#### Added

- `crates/secantus-bench`, a new WiredTiger-free workspace member shipping two
  binaries: `do-cluster` (provisioning, deployment, orchestration, reporting
  and teardown against the DigitalOcean v2 API) and `do-client` (the load
  agent and the server-side resource sampler).
- A log-linear latency histogram that merges by adding counts, so one report
  combines every worker across both client droplets into a single set of
  percentiles without shipping raw samples between machines.
- `invoke do-bench` / `do-up` / `do-deploy` / `do-run` / `do-suspend` /
  `do-status` wrappers, and a full operator guide at `bench/DO_CLUSTER.md`.
- Three teardown modes. The default is `destroy`, because a *powered-off*
  DigitalOcean droplet still bills at full price; `snapshot` keeps the
  installed software as a cheap image so the next run skips deployment, and
  `power-off` trades cost for a fast resume. `do-status` prints the live
  hourly rate from the API rather than a hardcoded price table that would go
  stale and quietly lie.
