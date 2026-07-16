### SQL server: composite types materialize — row(), record casts, typed field access

Composite values were half-real: a `'(foo,42,3.14)'::testcomp` cast passed raw
text through with a text OID, so psycopg's `register_composite` loaders never
fired, `row(…)` didn't exist, and `(value).field` access failed on anything
but a table column. The whole path is now materialized: `row(a, b, …)` builds
an anonymous record (rendered `(a,b)`, described as RECORD 2249, with the PG
binary record layout on binary cursors); casts to a declared composite parse
the record text literal — including quoted/escaped fields and nested records —
into the typed, field-named subdocument; a parameter a registered psycopg
dumper declares with the minted composite OID round-trips in both text and
binary formats; `array[…::testcomp]` describes with the paired array OID;
`pg_typeof` prints the type's name; and `('…'::testcomp).bar` types as the
declared field, not text.

Composite and domain OIDs also switched to the allocation-stable mint that
enums got earlier (assigned at `CREATE TYPE`/`CREATE DOMAIN` from a persisted
counter, never renumbered or reused) — positional minting shifted every type's
OID whenever a lexically-earlier name appeared, sending registered client
loaders decoding the wrong type. `oid::regtype` output now also double-quotes
reserved words (`"order"`), which psycopg's `sql.Literal` pastes verbatim.
psycopg's `tests/types/test_composite.py` goes from 66 failing to 17 (the
remainder: binary record edge samples and suite-order effects).

#### Added

- `scalar.py`: `row(…)` anonymous records; composite cast materialization
  (`_composite_from_text` / `_composite_from_seq` with positional remap for
  `row(…)::type`); `typemap.parse_pg_record_literal`.
- `pgextended.py`: PG binary record encode (`_encode_record`) and param decode
  (`_binary_record_to_text`); minted user-type binary params keep raw payloads
  until the catalog resolves them at Bind; `pg_typeof($N)` resolves minted
  user-type OIDs.
- `catalog.py`: allocation-stable `composite_type_oids` / `domain_type_oids`
  (shared `_mint_user_type_oid` counter machinery).

#### Fixed

- `planner.py`: constant-select RowDescription overrides for composite casts
  (minted OID, `composite` tag), `array[…::testcomp]` (paired array OID), and
  composite field access (the field's declared tag); user-defined type names
  build as `udt` DataTypes in parameter substitution.
- `virtual.quote_type_name`: reserved words double-quote in regtype output.
