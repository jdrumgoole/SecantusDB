### The Python servers trim their log while they run

The Python store checkpointed when it closed, in a backup and in `fsync`,
and at no other time. WiredTiger removes a log file only after a
checkpoint, so a long-running `secantusd-py` or `secantusd-py-pg` kept
every log file it had written and replayed them all after a crash. The Rust
servers had the same gap and were fixed first.

#### Fixed

- A durable store checkpoints every 60 seconds. `SECANTUS_CHECKPOINT_SECONDS`
  sets the interval, as it does for the Rust servers, and `Storage` takes
  `checkpoint_seconds`; 0 means never.
- A store opened for tests (`SECANTUS_TEST_FAST_STORAGE=1`, or
  `durable=False`) does not checkpoint while open, as before.
