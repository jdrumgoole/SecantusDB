### The benchmarks compare against mongod as a one-member replica set

`secantusd-mdb` runs as a one-member replica set by default, so a default
write waits for the journal (`w: "majority"`) and change streams work. The
benchmarks compared it with a standalone `mongod`, which acknowledges at
`w: 1` without that wait, and the droplet run of 2026-11-18 reported 0.14x
of mongod's throughput for what was a difference in durability, not engine
speed. On one Mac, a replica-set `mongod` 8.2.11 took 16.4 ms per default
insert against `secantusd-mdb`'s 4.9 ms.

#### Changed

- `do-cluster` starts `mongod` with `--replSet secantus-bench` and initiates it before the run.
- `bench/compare_servers.py` measures every row against a one-member replica set `mongod`, so `change_stream_drain` is now measured in the interleaved passes with the rest.
- `bench/concurrency.py` starts `mongod` with `--replSet` and initiates it.
