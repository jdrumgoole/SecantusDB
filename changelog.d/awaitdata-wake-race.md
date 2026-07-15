### Change streams: awaitData wake no longer misses a write landing mid-getMore

A tailable `getMore` baselined its awaitData wake predicate on a fresh
oplog-tail snapshot taken *after* draining the change-stream producer. A
write landing in the gap between the drain and the wait was counted into
that snapshot and never tripped the predicate — the `getMore` slept its
full `maxTimeMS` with the event already in the oplog, surfacing it only on
the post-wait re-drain. On a loaded machine that pushed delivery past the
client's await window (seen as a one-off
`test_await_data_blocks_then_wakes_on_insert` failure in the durable CI
lane). The predicate now baselines on the producer's own consumed position
(`entry.position_seq`, which the drain advances to the tail it actually
observed) — any write after that observation wakes or skips the wait,
mirroring the Rust server's `wait_for_oplog(position, ...)`, which was
never affected. A regression test pins the interleaving deterministically
by landing an insert inside the former race window. A side benefit: a
resuming cursor that drains a full filtered batch no longer sleeps its
whole `maxTimeMS` before fetching the next backlog page.

#### Fixed

- `commands.py` tailable `getMore`: wake predicate compares the oplog tail
  against `entry.position_seq` instead of a post-drain tail snapshot.
