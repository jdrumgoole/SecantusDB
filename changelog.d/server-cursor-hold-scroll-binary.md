### Rust PostgreSQL server: WITH HOLD / NO SCROLL enforcement and binary server cursors

The Rust `secantusd-pg` server now enforces the two cursor contracts a psycopg
`ServerCursor` leans on. A cursor declared `WITHOUT HOLD` is closed at COMMIT
(a later `FETCH` answers `34000 cursor does not exist`), a `WITH HOLD` cursor
survives it with its position intact, and ROLLBACK closes every cursor,
holdable included — where before all cursors silently outlived the transaction
that made them. A `NO SCROLL` cursor now rejects any backward `FETCH`/`MOVE`
with `55000 cursor can only scan forward`, matching the server it emulates.

Binary server cursors return real binary rows. A server cursor materialises its
rows once, in text, at DECLARE — but psycopg requests BINARY on the FETCH, not
on the DECLARE, and the frozen text bytes cannot be turned back into binary. The
server now keeps the resolved per-column values behind a `SELECT`-sourced cursor
and re-encodes them in the FETCH's format through the same codec the live query
path uses, so a binary `FETCH FORWARD` decodes correctly (before it handed the
client text bytes tagged as binary, which decoded to garbage). The FETCH's
row description reports the binary format to match.

Along the way `select generate_series(1, 2)::int4` — a cast applied to a
set-returning function in a FROM-less target list — is planned instead of
refused, carried as an ordinary per-row cast with the described column type
taken from the cast. A WHERE clause over such a series is now refused rather
than silently dropped, the same contract the `FROM generate_series(...)` form
already held.

#### Added

- `crates/secantus-pgserver`: server cursors keep the resolved typed values of a
  `SELECT` source so a BINARY `FETCH` re-encodes them in binary; the FETCH's
  `RowDescription` reports the requested format.
- `crates/secantus-pgplan`: `select generate_series(...)::type` (a cast over a
  FROM-less set-returning target) is planned as a per-row cast.

#### Fixed

- `crates/secantus-pgserver`: COMMIT closes non-holdable cursors and ROLLBACK
  closes all cursors, so a `WITHOUT HOLD` cursor is correctly invalid after
  COMMIT (`34000`) and cannot read discarded rows.
- `crates/secantus-pgserver`: a `NO SCROLL` cursor rejects a backward
  `FETCH`/`MOVE` with `55000 cursor can only scan forward`.
- `crates/secantus-pgplan`: a WHERE clause over a FROM-less `generate_series`
  target is refused (`0A000`) instead of being silently ignored, which had
  returned rows the client asked to exclude.
