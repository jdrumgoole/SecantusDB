### SQL server: type modifiers on the wire, E-string fidelity, transaction characteristics

RowDescription now carries real PG type modifiers: `select null::varchar(42)`
describes with typmod 46 (and varchar/bpchar keep their distinct OIDs
1043/1042 instead of folding onto text), `numeric(p,s)` packs precision and
scale — including negative scales, which sqlglot can't parse and the engine
now pre-rewrites through a sentinel — and the bit/varbit/time-family
precisions all flow through, so psycopg's `Column.display_size` /
`precision` / `scale` / `type_display` report like real Postgres. psycopg's
`test_column.py` goes 35 failed → 0.

Two more conformance holes close alongside. `E'…'` escape strings
interpolated by psycopg's ClientCursor (any string containing a backslash)
were double-unescaped — sqlglot already decodes the simple escapes, and the
second pass corrupted `\\b` into a backspace or raised 0A000 in the INSERT
value path; the decoder now finishes only the octal/hex/unicode forms
sqlglot leaves raw. And transaction characteristics are honoured end to end:
`BEGIN ISOLATION LEVEL … / READ ONLY / DEFERRABLE` (every spelling,
including the ones sqlglot rejects), `SET TRANSACTION`, and `SET SESSION
CHARACTERISTICS AS TRANSACTION` apply to the transaction (via the SET LOCAL
revert machinery) or the session defaults, and the `transaction_*` GUCs
mirror their `default_transaction_*` values until overridden — psycopg's
`set_isolation_level` / `set_read_only` / `set_deferrable` suite goes 13
failed → 0.

#### Added

- `result.py` / `pgwire.py`: `ColumnDesc.typmod` carried through both the
  simple-query and extended-protocol RowDescription emitters.
- `typemap.py`: `cast_type_identity` — (oid, typmod) for modifier-bearing
  cast targets, including arrays (element typmod with the array OID).
- `engine.py`: `_parse_txn_characteristics` shared by BEGIN / SET
  TRANSACTION / SET SESSION CHARACTERISTICS; `session.get_setting` falls
  back dynamically from `transaction_*` to `default_transaction_*`.

#### Fixed

- `scalar._unescape_estring` no longer re-decodes escapes sqlglot already
  resolved (the `test_leak` corruption); `planner._literal` accepts
  ByteString values in INSERT position.
