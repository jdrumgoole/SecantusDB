### The PG gate refuses a Backlog-ID this branch did not reserve

#### Fixed

- Reserving IDs on origin (`tools/backlog.py ids`) only helps when it is
  used. On 2026-11-09 a pg-batch lane numbered two new items RP-731 and
  RP-732 by hand. Another lane had reserved both, and nothing checked:
  the lane's gate passed, the other lane landed its own RP-732 beside it,
  and snapshot PR #2580 went red on `test_backlog_markers`, one CI cycle
  later.
- New `python tools/backlog.py check-ids [--file F]`, run by
  `scripts/gate_pg.sh` as a cheap step (`backlog-ids`), before any build.
  For every ID the branch adds that is not on origin's main, it requires:
  - a reservation under `refs/backlog-ids/<ID>`;
  - made on this branch, or else the same item (headline and `Verify:`)
    already held under that ID by another branch on origin, which is how a
    lane cut from pg-batch inherits the other lanes' items.
  It also refuses an ID that appears twice in the file. It exits 1 with one
  line per problem, naming the branch that holds the reservation.
- Tests: `tests/test_backlog_ids.py` (own reservation, another branch's,
  never reserved, inherited, already on main, duplicate). Run against the
  live origin, a lane cut from pg-batch passes, and hand-written IDs are
  refused.
- `scripts/gate_rm.py` does not run it yet; the MongoDB session can add
  `check-ids --file backlog-rust-mongodb.md` there.
