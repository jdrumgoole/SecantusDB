### The three-droplet benchmark now measures SecantusDB against real MongoDB

The DigitalOcean harness gained the comparison it existed for. `--engine both`
(the new default) installs MongoDB Community from MongoDB's own apt repository
alongside `secantusd-rs`, runs the identical workload against each in turn on
the *same* droplets, and prints them side by side with a ratio.

Everything that could confound the comparison is held fixed: the same cores,
the same clients, the same private network, the same operation mix, the same
WiredTiger cache size — one `--cache-size` drives SecantusDB's `--cache-size`
and mongod's `--wiredTigerCacheSizeGB` — and an empty data directory for each.
The engines run sequentially, never side by side, because two databases
sharing four cores would measure contention rather than either engine.

The first live comparison, on one `c-4` server and two `c-2` clients with 8 KiB
documents and a 70/20/10 insert/find/update mix:

| engine | version | ops/s | p50 ms | p99 ms | p99.9 ms | server CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| secantusdb | 0.5.3-beta.160 | 8,244 | 2.33 | 25.07 | 114.03 | 83.2% |
| mongod | v8.0.29 | 17,984 | 1.49 | 6.94 | 11.58 | 84.5% |

**SecantusDB reaches 0.46x MongoDB's throughput on this workload**, and the gap
widens sharply in the tail — 3.6x at p99 and 9.8x at p99.9. Both engines
saturated the same server while the clients idled at 12-15%, so both figures
are server-bound and the comparison is fair; a second back-to-back pass
reproduced the ratio exactly, with each engine within 1% of its first result.

#### Added

- `--engine both | secantus | mongod` and `--mongod-version` (default `8.0`).
- A `comparison.md` artifact per run, and per-engine summaries, client reports,
  resource traces and journals alongside it.
- Ratios reported in their own senses — throughput above 1.0 is faster,
  latency below 1.0 is quicker — because conflating those directions is how
  benchmark tables mislead.

#### Fixed

- The harness's state and results directories were resolved relative to the
  current directory, so running it through `cargo` from `crates/` created a
  second `bench/.do-state` there (one copy reached git). Both now anchor to the
  repository root.
