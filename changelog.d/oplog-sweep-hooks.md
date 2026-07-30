### Oplog experiment hooks and the sweep that found 54%-retention durable writes

Three measure-oriented env hooks land on the Rust server so oplog append-path
experiments no longer need a rebuild: `SECANTUS_OPLOG_SHARDS` overrides how many
shard tables the write path routes across (1–16; reads always consider all, so
any store stays fully readable), `SECANTUS_OPLOG_TABLE_EXTRA` appends
last-key-wins WiredTiger config to the oplog/preimage table creates (the
`SECANTUS_WT_CONFIG_EXTRA` trick at table scope), and `SECANTUS_DATA_NONLOGGED`
is a loudly-documented, crash-unsafe, measure-only probe of the mongod
architecture (journal only the oplog). `bench/oplog_sweep.py` drives the arms
interleaved and reports retention against the same-session no-oplog ceiling.

The sweep's headlines (recorded as Finding 13): the 16-way oplog sharding is
now pure overhead — every lower shard count beats it at eight writers;
turning oplog compression off craters throughput to 19% retention (zlib is
load-bearing under write pressure); cache size is the strongest single knob;
and the winning stack (2 shards + append-tuned oplog pages + 4G cache) reaches
**102.8k docs/s at eight writers fully durable — 54% of the no-oplog ceiling**,
up from 43% on the defaults. The mongod-architecture probe adds only the last
+11% on top (60%, matching mongod's own 61% oplog-retention ratio), so the
replay-on-open recovery project is parked until the config winners ship.

#### Added

- Rust server: `SECANTUS_OPLOG_SHARDS`, `SECANTUS_OPLOG_TABLE_EXTRA`, and
  `SECANTUS_DATA_NONLOGGED` (measure-only, crash-unsafe) experiment hooks —
  all default-off, create/routing-time only.
- `bench/oplog_sweep.py`: the interleaved oplog append-path sweep runner.
