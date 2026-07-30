### The Finding-13 winners become the defaults — another +14% at eight writers, no knobs required

The oplog append-path sweep's measured winners now ship as defaults on the
Rust server. Oplog writes route across two shard tables instead of sixteen —
the wide split existed to spread a rightmost-page append hotspot that the
RecordId keying and the prune fix eliminated, and the sweep measured every
narrower width beating sixteen; the read side still scans all sixteen, so
stores written under any width stay fully readable and interchangeable. The
oplog and pre-image btrees are created append-tuned (`split_pct=100,
leaf_page_max=128KB` — rows arrive in ascending seq order and are never
updated, so pages fill completely before splitting), and the daemon and the
Python `RustServer` handle raise their WiredTiger cache default from a 1G to
a 4G *cap* — WiredTiger fills cache lazily, so idle test servers stay as
small as before while sustained writers stop thrashing eviction
(`--cache-size` / `cache_size=` still override; the low-level
`Storage::open` library default is unchanged).

Interleaved A/B against the previous defaults on the reference box: sync
single-writer 31.8k → 35.1k docs/s (+10%), eight writers 78.1k → 88.7k
(+14%) — on top of the prune-fix release's +62%. Oplog block compression
stays on deliberately: the sweep measured turning it off cratering
throughput to a fifth of the ceiling (bigger uncompressed pages mean more
eviction IO, and IO volume — not CPU — is the constraint).

#### Changed

- Rust server: oplog write routing defaults to 2 shard tables (was 16);
  `SECANTUS_OPLOG_SHARDS` still overrides 1–16; reads scan all tables
  regardless, so on-disk compatibility is unaffected.
- Rust server: oplog/pre-image tables are created with
  `split_pct=100,leaf_page_max=128KB` (fresh stores; existing stores keep
  their config).
- Rust server: `secantusd-rs` and the Python `RustServer` handle default
  `cache_size` to `4G` (a lazy cap, was `1G`); `docs/concurrency.md`'s
  tuning guidance updated (log `prealloc` now hurts at eight writers
  post-prune-fix; never disable oplog compression).
