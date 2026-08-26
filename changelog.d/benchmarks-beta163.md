### Tail latency improved again

Re-measured as part of cutting `secantusdb-v0.5.3-beta.163`, on dedicated cloud
instances against a real `mongod`.

#### Changed

- **p99.9 latency relative to mongod: 1.48× → 1.18×.** In absolute terms our
  p99.9 fell from 49.10 ms to 37.34 ms (−24%) while mongod's moved −5%, so the
  gain is ours rather than the reference moving. Throughput is flat at 0.74×
  (was 0.73×, inside the 3.1% pass spread).
- **Two per-operation rows now beat mongod** — indexed range 0.91×, full scan
  0.96× — with filtered scan and the change-stream drain at parity. The Rust
  server's overall range narrowed from 0.8×–2.9× to **0.9×–2.4×**.
- The reference moved from **mongod 8.0.29 to 8.0.31** (the gauge installs the
  latest 8.0.x). The results file records the version, so the change is
  traceable rather than silent.
