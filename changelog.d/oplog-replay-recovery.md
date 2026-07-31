### Log-only-the-oplog becomes crash-safe: replay-on-open recovery lands

The `SECANTUS_DATA_NONLOGGED` mode — the mongod storage architecture, where
only the oplog is WAL-journaled and the data tables are checkpoint-durable —
graduates from a measure-only benchmark probe to a recoverable
configuration. A periodic stable checkpoint (60s cadence, the mongod
default; `SECANTUS_CHECKPOINT_SECONDS` overrides) anchors a marker in the
always-logged oplog-meta table, the oplog prune never touches entries above
the marker (they are the recovery source), and `Storage::open` replays the
oplog above the marker through the ordinary write paths — idempotently, so
the deliberately conservative marker can never double-apply work. A clean
close anchors a final checkpoint even under the fast-storage test
environment, whose skip-the-close-checkpoint optimisation would otherwise
lose unlogged tables' data with no crash involved.

The contract is proven by a hard-kill harness
(`tests/test_crash_recovery.py`): a writer subprocess is `SIGKILL`ed
mid-load and every acknowledged write must be present after the reopen —
including with no checkpoint ever taken, where the entire dataset comes
back from oplog replay alone. Durability matches the logged default at
each `sync_on_commit` setting: with per-commit fsync every acknowledged
write survives a hard kill; without it, a hard crash can lose the unsynced
WAL tail — in either mode, exactly as before.

The default is unchanged, and deliberately so: with the durability
anchoring live, the mode's own measurements moved. A single writer gains
~5%, and a workload whose oplog stays under the retention cap keeps the
probe-era headroom (~122k docs/s at eight writers measured with anchoring
idle) — but a sustained eight-writer load at cap pressure pays the
periodic checkpoint of a hot, unlogged working set and lands at roughly
half the logged default's throughput. Finding 14 records the decomposition;
the default flip stays parked until the checkpoint cost is tamed. The mode
is correct and recoverable today; choose it for read-heavy, single-writer,
or bounded workloads.

#### Added

- Rust server: replay-on-open crash recovery for `SECANTUS_DATA_NONLOGGED`
  stores — stable-checkpoint marker + periodic checkpoint thread +
  idempotent oplog replay + prune clamp; the mode is recorded per-store at
  create time (existing stores are unaffected by the env var).
- `tests/test_crash_recovery.py`: the hard-kill recovery harness (SIGKILL
  mid-load → reopen → every acknowledged write present), plus WT-level
  stable-marker tests.
