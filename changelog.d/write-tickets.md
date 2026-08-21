### `--write-tickets`: admission control for storage-engine writes

A new server flag bounds how many writes are inside WiredTiger at once, with
the rest queueing outside it — MongoDB's ticket system in miniature. Off by
default (`0`), so a server that does not opt in behaves exactly as before.

It was built to test a specific hypothesis about the p99.9 tail gap against
MongoDB, and the honest result is that **it does not fix it**. Three
interleaved passes under maximum cache pressure, 16 concurrent writers:
`--write-tickets 4` cost 22% of throughput to buy 12% off p99.9 and 32% off
the worst single stall. At a larger cache the benefit shrank to nothing, and
two tickets made the tail worse.

Capping engine concurrency relocates the queue rather than removing it: a
client with sixteen requests in flight still waits for all sixteen, whether
they are stalled inside WiredTiger's eviction or parked in a condvar. The flag
therefore stays off by default and is documented as a diagnostic and a
predictability knob — with tickets the tail became perfectly repeatable across
passes — rather than as a performance fix. The full investigation, including
the hypotheses it rules out, is in `tasks/backlog.md`.

#### Added

- `--write-tickets N` on `secantusd-rs`, and `write_tickets` under `[storage]`
  in the config file.
- `secantus-storage::admission`, a permit pool with a thread-local re-entrancy
  guard so a multi-document transaction rides its outer ticket instead of
  deadlocking against itself, and RAII release so a panicking write cannot leak
  a permit and wedge the pool.
