### Four built-in types were missing from `pg_type`

`char`, `json`, `pg_lsn` and `txid_snapshot` had no `pg_type` row, so a client
enumerating types never saw them. pgjdbc's `getTypeInfo()` is checked against a
fixed list of 38 built-in names, and those four were the gap.

`char` is worth calling out: it is PostgreSQL's internal **one-byte** character
type (oid 18), a different type from `bpchar` / `character(n)` (1042). The names
invite conflating them, so the test pins both rows.

Oids, `typarray` and `typlen` all measured against PostgreSQL 14 on 2026-09-27.

#### Added

- `pg_type` rows for `char` (18), `json` (114), `pg_lsn` (3220) and
  `txid_snapshot` (2970).

#### Noted, not fixed

Probing this surfaced a separate divergence: a `json` column reports `atttypid`
3802 (`jsonb`) rather than 114. The two are distinct types in PostgreSQL — `json`
keeps its text verbatim, `jsonb` normalises — and this server folds `json` onto
the `jsonb` tag. The `pg_type` row added here is correct on its own terms, since
PostgreSQL has both; only the column identity is wrong. Filed in
`tasks/backlog.md` rather than fixed, because reporting 114 over jsonb storage
would trade one wrong answer for another — the value semantics are the harder
half.

Found from pgjdbc's `DatabaseMetaDataTest::types`.
