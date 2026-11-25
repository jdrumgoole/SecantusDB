### Rust PG server: a COPY FROM ends its own xid and command at CopyDone

`secantusd-pg` now ends a COPY FROM's transaction state when its data
ends, as PostgreSQL does, so the statement after it no longer sees the
copied rows as its own command's.

#### Fixed

- After an autocommit `COPY ... FROM STDIN`, the next statement ran under
  the COPY's xid: `txid_current()` returned the copied rows' `xmin`, and
  an `INSERT ... ON CONFLICT DO UPDATE` over a copied row was refused
  21000 ("cannot affect row a second time") where PostgreSQL 15.19 updates
  it. The statement returns at CopyInResponse, before any row takes an
  xid, so the xid, the SSI registration and the row waits now end at
  CopyDone (or when the COPY fails) instead of with the next statement.
- Inside a transaction block the same 21000 came from the command id: the
  COPY's was advanced when its statement returned, so its rows took the
  next statement's `cmin`. A COPY FROM now uses its command id at CopyDone,
  and `cmin` across INSERT, COPY and INSERT in a block reads 0, 1, 2 as on
  15.19.
- Fixes RP-1069 (`tests/backlog/test_rp_found_rp693.py::test_copy_completes_its_own_xid`,
  `test_copy_in_a_block_uses_its_own_command_id`).
