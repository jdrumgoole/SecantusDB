### The Go gauge was hiding 16 failures behind a 100% score

It had been reporting **100.0%** over a run that never finished. `go test`
killing itself on `-timeout` panics the binary without emitting a terminal event
for the tests still in flight, so 476 of 481 reported and the summariser scored
the survivors as perfect. **A truncated run looks better the more tests go
missing**, which is why it survived review — in every committed report back
through 2026-09-21.

With the run completing: **594 passed / 16 failed / 49 skipped over 659 tests —
97.3%**, on both servers. The 16 failures are not new; they simply never ran.

#### Fixed

- `TestInitialDNSSeedlistDiscoverySpec/replica_set` excluded — the narrow branch,
  not the whole spec, whose `sharded`, `load_balanced` and non-RS cases all run.

  It blocks forever: `getServerByAddress` calls
  `topo.SelectServer(context.Background(), ...)` — no deadline — waiting for the
  SRV-resolved `localhost:27017` to join the topology. Our daemon binds an
  ephemeral port, so it never joins. The subtest is gated on the replica-set
  persona this gauge deliberately keeps for change streams, so `--standalone`
  cannot dodge it the way it does for the C gauge.

  What is lost: the Go driver's own SRV/TXT resolver. No SecantusDB code path.
  Measured: 803 started / 798 finished / 5 hung before, **659 / 659 / 0 in ~6
  minutes** after — the timeout had also been costing 30 minutes per run.

#### Found

Eight distinct leaf failures, triaged in `tasks/backlog.md`. Two are worth
naming here: **`setParameter` is not implemented on either server** (mongod has
it), and **`replSetStepDown` answers 59 where a standalone mongod answers 76
`NoReplicationEnabled`** — a divergence rather than an unimplemented feature. Two
more, the SDAM pool-clearing pair, assert the pool is *not* cleared on a timeout
or cancelled context, and look most likely to be real.
