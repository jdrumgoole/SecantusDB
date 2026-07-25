### Rust server: a raw WiredTiger config escape hatch for write-throughput tuning

The Rust server (and `secantusd-rs` daemon) now honour `SECANTUS_WT_CONFIG_EXTRA`,
which appends raw WiredTiger connection-config to the string SecantusDB builds — an
ops/tuning hook to change WiredTiger knobs without recompiling. WiredTiger's config
parser takes the last occurrence of a duplicated key, so an appended clause overrides
the corresponding default (e.g. `SECANTUS_WT_CONFIG_EXTRA="cache_size=4G,log=(file_max=512MB,prealloc=true)"`).
An invalid key fails loudly at startup rather than being ignored, and the default
(no variable set) is byte-for-byte unchanged.

This landed alongside a write-ceiling tuning sweep (`tasks/rust-perf-findings.md`
Finding 6): on eight concurrent writers, log pre-allocation lifts the aggregate
write ceiling ~8%, a larger cache lifts single-op (read-modify-write) throughput
~27% while barely moving the pure-insert ceiling, and more eviction threads do
nothing — each a modest, resource-costed lever, so the hatch exposes them for
per-deployment tuning rather than changing daemon defaults.

#### Added
- `SECANTUS_WT_CONFIG_EXTRA` (Rust server / daemon): raw WiredTiger connection-config
  appended to the built config string, overriding defaults via last-key-wins.
