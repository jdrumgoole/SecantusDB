### The Rust servers trim their log while they run

Neither Rust server checkpointed between opening its store and closing it,
and WiredTiger removes a log file only after a checkpoint. A server under
writes kept every log file it had ever written, and after a crash replayed
all of them.

#### Fixed

- `secantusd-rs` and `secantusd-pg` checkpoint every 60 seconds
  (`SECANTUS_CHECKPOINT_SECONDS`, and `--checkpoint-seconds` on
  `secantusd-rs`). Measured on `secantusd-pg` with eight clients updating for
  13 minutes: the store was 2.1 GB before this change and 69 MB after, and a
  restart after a SIGKILL took 36.8 s before and 1.4 s after. Throughput was
  the same, 13,815 statements a second before and 13,864 after.
- A store opened for tests (`SECANTUS_TEST_FAST_STORAGE=1`, or
  `durable=false`) does not checkpoint while open, as before.
