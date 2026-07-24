### Oplog pruning no longer scans the whole oplog on every write burst

The Python server pruned its oplog by reading and BSON-decoding **every**
entry, once per thousand writes, just to find the handful that had aged out
or spilled over the entry cap. On a small oplog that was invisible; once a
workload had filled the oplog to its 100,000-entry cap — a single large bulk
insert does it — each prune decoded 100,000 rows and dropped almost none,
about nine tenths of a second of pure waste, firing again every thousand
subsequent writes. Because one server backs a whole test session, that tax
landed on every later write in the session: a workload timed at under half a
second against a fresh server took five to twelve seconds once the oplog was
full.

The prune now does work proportional to the number of rows it actually
deletes, not to the size of the oplog. Sequence numbers are minted in lockstep
with timestamps, so both prune criteria — entries older than the retention
window, and entries beyond the count cap — only ever remove an oldest-prefix.
The server keeps a running count of live oplog rows (seeded once at open, then
maintained on every emit and prune) and streams the oldest entries in order,
stopping at the first one that neither criterion condemns. A prune that drops
a hundred rows now reads about a hundred and one, whatever the oplog's size.

A single prune over a full 100,000-entry oplog drops from ~0.86s to ~0.002s,
and the same fresh-server workload that degraded to five-plus seconds after a
large insert stays at under half a second. In the pymongo conformance gauge,
which runs a 200,000-document bulk-insert test partway through, the change
removes ~27s of accumulated prune tax from the run — most visibly the tests
that happen to run just after the oplog fills.

#### Changed

- `secantus/storage.py`: `_prune_oplog_locked` streams only the oldest oplog
  entries and stops early instead of scanning and decoding the entire oplog;
  a new in-memory `_oplog_live_count` (seeded by a one-time key-only count on
  open) drives the entry-cap decision without a counting scan. Doomed rows are
  deleted from the one table the oldest-first walk found them in, not probed
  across all shard tables.
