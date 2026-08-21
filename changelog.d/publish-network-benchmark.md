### Publish the three-droplet MongoDB comparison, measured honestly

`docs/benchmark.md` carried only single-host loopback numbers, where the load
generator competes with the database for the same cores. It now also carries
the deployment-shaped measurement: one server droplet, two separate client
droplets, real NICs between them, and **SecantusDB run back-to-back against a
real `mongod` on the same hardware**.

The number is not flattering, and it is the one we would rather publish. On
8 KiB **incompressible** documents with a 70/20/10 mix, three interleaved
90-second passes on a `c-4` server:

| engine | ops/s | p50 | p99 | p99.9 |
| --- | ---: | ---: | ---: | ---: |
| SecantusDB 0.5.3-beta.160 | 3,993 | 2.41 ms | 64 ms | **1,303 ms** |
| mongod 8.0.29 | 14,937 | 1.73 ms | 10 ms | **18 ms** |

**A quarter of MongoDB's throughput, and 72x the p99.9 latency.** Both engines
saturated the same server while the clients sat idle, so both figures are
server-bound and the comparison is fair; run-to-run spread was under 4%.

This supersedes an earlier 0.46x figure that was measured with the harness's
default repeated-character payload. Both engines compress that away, and it
flattered SecantusDB — MongoDB's snappy-compressed journal benefits far more
than SecantusDB's uncompressed one. Re-measuring on incompressible data before
publishing was the difference between a citable number and a misleading one.

The docs state the weak point plainly: SecantusDB's p50 is within 1.4x of
MongoDB's, so typical operations are competitive, but under sustained write
saturation the worst 0.1% of operations are far slower — and if that matters
for your workload, run a real `mongod`. Two identified, unfixed,
configuration-level causes are linked from there.

#### Added

- A "Over a real network, against a real MongoDB" section in
  `docs/benchmark.md`, reproducible with `invoke do-bench --repeat 3 --payload
  random`.
- `--payload` threaded through `do-cluster` and the `invoke do-bench` / `do-run`
  wrappers, so the orchestrator can select payload entropy (it previously
  existed only on the client agent).
