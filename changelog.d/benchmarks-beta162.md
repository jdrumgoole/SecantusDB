### Benchmark numbers now come from the release they describe

The published performance figures were re-measured as part of cutting
`secantusdb-v0.5.3-beta.162`, on dedicated cloud instances against
**mongod 8.0.29** — so for the first time they describe the build you can
actually download, rather than whatever was current when someone last
remembered to re-run them.

#### Changed

- **Tail latency improved substantially**: p99.9 against mongod moved from
  2.04× to **1.48×** on the head-to-head. Throughput is flat at 0.73× (was
  0.75×, inside the 2.1% run-to-run spread).
- **Three per-operation rows now beat mongod** — filtered scan 0.85×, indexed
  range 0.92×, change-stream drain 0.93× — and full scan is at parity.
- Concurrency figures are now measured on the same cloud instance as
  everything else rather than a workstation. Absolute throughput is lower
  because those cores are slower; the scaling ratios the page reports are
  better, and `mongod` — the control — scales 4.96× there against 4.65× on the
  workstation, confirming the instance is not core-starved.
