### A kill -9 crash window in the data-nonlogged mode could lose acknowledged writes — fixed

The opt-in log-only-the-oplog mode (`data_nonlogged`) wrote its stable
marker — the seq recovery replays from — *before* running the checkpoint it
describes. The marker lives in an always-WAL-logged table, so it became
crash-durable immediately: a `kill -9` landing after the marker's WAL write
but before the checkpoint completed recovered with a marker *above* what the
last checkpoint actually contained, and replay started too high — every
acknowledged write between the old checkpoint and the marker was silently
lost as a mid-history hole (the oplog rows themselves all survived). The
window is a few milliseconds on an idle machine but stretches with checkpoint
duration under load, which is how the hard-kill harness caught it live: 2,300
of 7,200 acknowledged documents missing after recovery, with all 7,200 oplog
entries present.

Both checkpoint sites (the periodic anchor thread and explicit/close-time
`stable_checkpoint`) now checkpoint first and write the marker after. A crash
between the two leaves the *old* marker, and replay covers extra
already-applied entries — the idempotent-replay path that has always existed
absorbs exactly that. Stale-marker is safe; eager-marker loses data. The
hard-kill harness also gained self-diagnosis: on any future loss it reports
whether the missing documents' oplog entries survived, separating WAL loss
from replay-window bugs at a glance.

#### Fixed
- `secantus-storage`: stable-marker row written after (not before) its
  checkpoint in both the periodic checkpoint thread and `stable_checkpoint`;
  the recovery floor can now only ever be conservative.
- `tests/test_crash_recovery.py`: loss assertions carry a diagnosis dict
  (doc count, oplog row count and tail, whether the first missing id's oplog
  entry exists).
