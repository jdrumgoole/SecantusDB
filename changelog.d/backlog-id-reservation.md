### Backlog IDs are reserved on origin before they are written

#### Fixed

- `tools/backlog.py ids` gave a new item "one past the highest ID in this
  checkout". The pg-batch lanes branch from main when a batch starts, and
  another session files on main, so neither saw the other's new IDs for
  hours. On 2026-11-08, RP-571 and then RP-606 were each handed out twice.
  The second pair reached main, where `test_backlog_markers` failed for
  every PR until one was renumbered by hand.
- `ids` now reserves each new ID on origin before writing it, by pushing a
  parentless commit to `refs/backlog-ids/<ID>`.
  - A second claim on the same ref is rejected as non-fast-forward, so the
    tool moves on to the next number.
  - Each reservation commit carries a random nonce. Without it, two claims
    made in the same second by the same identity could produce identical
    commits, and both pushes would succeed.
  - The starting number also counts every reserved ref and the IDs in each
    origin branch's copy of the file, which covers IDs filed by older
    copies of the tool.
- With no network, or no push access, `ids` refuses and writes nothing.
  Tests: `tests/test_backlog_ids.py` (a bare repository stands in for
  origin).
