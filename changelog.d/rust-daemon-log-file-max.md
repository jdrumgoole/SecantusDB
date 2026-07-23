### Rust server: bigger WAL log files for higher write throughput

The standalone `secantusd-rs` daemon now defaults its WiredTiger write-ahead-log
`file_max` to **2GB** (up from WiredTiger's 128MB default), configurable with a new
`--log-file-max` flag and a `[storage] log_file_max` config key. The 128MB default
forced WiredTiger to rotate and allocate a fresh log file constantly under a
multi-writer write load, and that rotation overhead was a measurable throughput
tax: on an idle machine, raising `file_max` to 2GB measured **~+13-19%** insert
throughput at 4-8 concurrent writers (sustained 60s: +13.5% at 8 writers; 20s
burst: +19-24%). 2GB is WiredTiger's hard cap for log files; the log files stay
**sparse** (`prealloc=false`), so a small workload still costs only the bytes it
actually writes, not the full `file_max`.

This lifts the whole write-throughput curve cheaply; it does not change the
multi-writer *scaling* shape (that is bounded by per-document write amplification,
tracked separately). The embedded in-process handle keeps the 128MB log default —
a test suite spins up many ephemeral instances and must not each carry a large
sparse log.

#### Added
- `secantusd-rs --log-file-max SIZE` and `[storage] log_file_max` in the config
  file (unit-suffixed, e.g. `128MB` / `1GB` / `2GB`), threaded into the WiredTiger
  connection config. Daemon default: `2GB`.

#### Changed
- The daemon's WiredTiger WAL `log=(file_max=...)` defaults to 2GB instead of
  128MB. `wt_config` takes the value as a parameter; the embedded `RustServer`
  handle and `Storage::open`'s test-default config are unchanged at 128MB.
