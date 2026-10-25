### The Rust PostgreSQL server: SERIALIZABLE is serializable

Until now `secantusd-pg` ran SERIALIZABLE transactions as REPEATABLE READ:
snapshot isolation, which lets write skew commit. Two transactions could each
read both on-call doctors and take themselves off call, and both would commit.
SERIALIZABLE is now PostgreSQL 15's Serializable Snapshot Isolation, a port of
`predicate.c`. Reads take SIREAD locks on the relation, page or tuple the
planner's chosen scan reads. Writes check them for read/write conflicts, and a
transaction at the pivot of a dangerous structure fails with 40001 and
PostgreSQL's reason code. `READ ONLY` transactions get the safe-snapshot
optimisation, and `DEFERRABLE` waits for a safe snapshot.

The check is PostgreSQL's own isolation suite. Of the 27 serializable specs
from 15.19's suite run here, 18 (448 permutations) replay step for step
against traces recorded on PostgreSQL 15.19. Four more items are filed open: partial-index, GIN, GiST and hash scan
granularity; a duplicate-key insert under REPEATABLE READ or SERIALIZABLE; and
a million-row transaction.

#### Added

- `secantus-pg`: Serializable Snapshot Isolation (`ssi.rs`,
  `serializable.rs`). Fixes RP-061
  (`tests/backlog/test_rp_isolation.py::test_ssi_spec`,
  `tests/backlog/test_rp_found_2026_10_20.py::test_serializable_write_skew`).
  - The scan a read's SIREAD locks follow is chosen by
    `secantus-pgplan`'s new `costsize::scan_paths` (seq, bitmap or index
    path, with `enable_*` off costed as `disable_cost`).
  - An index scan's leaf pages come from the B-tree page model
    (`btree_model.rs`, `scan_pages`). That model now also reports the leaf
    each insert lands on and each page split, for `PredicateLockPageSplit`.
- `secantus-storage` (shared with the MongoDB server):
  `UserTransactionHandle::began`, whether a transaction has taken its
  WiredTiger snapshot yet.
- `tools/probes/pg_isolation.py`: runs PostgreSQL isolation specs against two
  servers, or records one server's traces (`--record`).

#### Fixed

- A REPEATABLE READ or SERIALIZABLE transaction took its snapshot at a SET,
  SHOW or LOCK. It now takes it at the first statement that needs one, as
  PostgreSQL does. Fixes RP-260
  (`tests/backlog/test_rp_isolation.py::test_snapshot_at_first_query`).
- `INSERT ... ON CONFLICT DO UPDATE ... WHERE` now locks the conflicting row
  before it tests the WHERE, so a concurrent UPDATE waits even when the WHERE
  rejects the row. Fixes RP-257
  (`tests/backlog/test_rp_isolation.py::test_on_conflict_where_locks_the_row`).
- A void SQL-language function answers NULL. A void PL/pgSQL function still
  answers the empty string. Fixes RP-258
  (`tests/backlog/test_rp_batch75.py::test_sql_void_function_is_null`).
- RAISE now follows `exec_stmt_raise`, in DO blocks and functions alike. Fixes
  RP-259 (`tests/backlog/test_rp_batch75.py::test_raise_condition_and_options`).
  - `RAISE SQLSTATE '…' USING MESSAGE = …` is accepted.
  - A condition's name is the message only when nothing else gives one.
  - `USING ERRCODE` alone gives the code or the condition's name as the
    message.
  - A repeated ERRCODE, MESSAGE, DETAIL or HINT is 42601.
