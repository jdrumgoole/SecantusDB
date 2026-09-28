### Sequences and identity columns on the Rust PostgreSQL server

`CREATE SEQUENCE`, `nextval`, `currval`, `setval`, `ALTER SEQUENCE`,
`DROP SEQUENCE` and `GENERATED ... AS IDENTITY` were all missing. The machinery
was already there — `serial` columns have drawn from a stored sequence
document since the beginning — but none of it was reachable from SQL.

Against a live PostgreSQL 14.13 the sequences corpus went from 24 divergences
of 26 to 1, and the one left is `information_schema.sequences`, part of the
catalog-introspection gap. A second corpus of 70 lines written for this change
is clean.

A sequence is also a RELATION, so `SELECT last_value, is_called FROM s` reads
it like a one-row table, and `pg_get_serial_sequence('t', 'id')` names the
sequence a serial column owns.

Three behaviours were measured rather than assumed, and each is a place a
plausible implementation goes wrong:

**The bound a sequence runs into depends on its DIRECTION.** A descending
sequence starts at its MAXIMUM and exhausts at its MINIMUM. Checking only
`max_value` let `CREATE SEQUENCE s INCREMENT -3 MINVALUE -10` run past its
floor for ever, and named the wrong bound when it did stop. `CYCLE` wraps to
the far bound instead of failing.

**`currval` is keyed to the SESSION**, not to the sequence. It is `55000`
before any `nextval` in that session even when another session has advanced
the sequence — reading the stored value instead would hand one session
another's number.

**The identity overriding matrix has four live cases**, measured on 14.24:
`GENERATED ALWAYS` refuses a hand-written value with `428C9`;
`OVERRIDING SYSTEM VALUE` lets it through; `OVERRIDING USER VALUE` discards it
and draws from the sequence for EITHER kind; and an explicit NULL is a
not-null violation for both kinds, even under `OVERRIDING SYSTEM VALUE` —
the override decides whose value wins, not whether the column may be null.

#### Added

- `CREATE` / `ALTER` / `DROP SEQUENCE` with `START`, `INCREMENT`, `MINVALUE`,
  `MAXVALUE`, `CYCLE`, `RESTART [WITH]`, `OWNED BY`, `IF NOT EXISTS` and
  `IF EXISTS`; `nextval` / `currval` / `setval` (both arities);
  `pg_get_serial_sequence`; and a sequence read as a relation.
- `GENERATED ALWAYS` / `BY DEFAULT AS IDENTITY` columns, with
  `OVERRIDING SYSTEM VALUE` and `OVERRIDING USER VALUE`.

#### Fixed

- `secantus-pgcatalog`: a column's catalog keys that this server does not
  model — `identity`, `enum_type`, `domain_type`, `generated`, `comment`,
  `default_expr`, `composite_type`, `json_plain` — were written back as
  unconditional NULLs, so any rewrite of a catalog row here ERASED what the
  Python server had recorded. That was unreachable while nothing rewrote an
  existing row; `ALTER TABLE` made it reachable last week. They are kept
  verbatim now, and `identity` is modelled outright because this server
  enforces it.
- `tools/probes/pg_corpora/sequences.setup.sql`: the identity tables were
  never dropped, so the REFERENCE server accumulated them across runs —
  `CREATE TABLE idt11` answered `42P07` and `count(*)` grew by three every
  time. Four scenarios were comparing against that debris rather than against
  PostgreSQL's behaviour.
