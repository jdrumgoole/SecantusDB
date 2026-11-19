### Faster startup: no checkpoint when opening has nothing to anchor

Opening a store took one or two checkpoints that had nothing to protect.
Both are now skipped when there is no work, which shortens every start of
`secantusd-mdb` and `secantusd-pg`.

#### Changed

- `recover_from_oplog` skips its re-anchoring checkpoint when nothing lies
  above the stable marker (a clean shutdown or a new store). The marker
  already names the tail, and the periodic stable checkpoint anchors it as
  soon as there is a gap.
- `upgrade_id_keys` skips the re-key pass and its checkpoint on a new
  store, which holds no keys in the old format. It still records the key
  format.

On a loaded M2 Pro, opening storage went from 1,432 to 1,032 ms on a cold
start and from 698 to 565 ms on a restart (medians of 12 interleaved runs,
from the server's own startup statistics).
