### SQL server: enum result OIDs in RowDescription, and real array OIDs for user types

An enum-typed result column used to describe itself as plain `text` (OID 25),
which broke the catalog-driven type registration flow every Postgres driver
builds on: psycopg's `EnumInfo.fetch` would find the type's minted OID in
`pg_type`, but no result column ever carried it, so `register_enum` loaders
never fired and enum values always came back as bare strings. `RowDescription`
now reports the same minted OID that `pg_type` / `pg_enum` / `pg_attribute`
reflect — the mint moved onto `Catalog.enum_type_oids` so reflection and the
wire layer cannot drift — and the full psycopg round-trip works: fetch the
type, register a Python `enum.Enum`, and SELECT / RETURNING rows come back as
enum members.

Chasing the conformance numbers surfaced a second, far larger bug: every
user-declared type (enum / domain / composite) reported `pg_type.typarray = 0`.
Clients key array-type registrations on that value, and 0 is `INVALID_OID` —
psycopg's own suite pops the loader registered under `array_oid`, which
deleted psycopg's *global unknown-oid fallback loader* and poisoned every
subsequent unknown-OID text load in the process. User types now mint a derived
paired array OID (`oid + 100000`).

Enum values also flow through expressions and parameters, not just table
columns, so the cast and Bind paths grew the same fidelity: `SELECT %s::mood`
describes with the enum OID and validates the label (`22P02 invalid input
value for enum` on a label the type doesn't have), a parameter a registered
psycopg dumper declares with the enum OID is label-validated at Bind,
`oid::regtype::text` quotes mixed-case type names the way real Postgres does
(psycopg's ClientCursor pastes that string verbatim as a cast suffix), and
`%s::mood[]` round-trips as a list through the minted array OID in both text
and binary formats. psycopg's enum-adaptation suite (`tests/types/test_enum.py`,
197 tests) passes completely. On the full psycopg conformance gauge the work
takes the headline from 2554 passed (61.9%) to 2900 passed (70.3%) under
deterministic test order — +346 tests, including the entire 212-test
"unknown oid loader not found" cluster and all 152 enum failures.

#### Added

- `catalog.py`: `Catalog.enum_type_oids(db)` — the single enum-OID mint,
  shared by `pg_catalog` reflection (`virtual._enum_oids` now delegates) and
  result-column description. OIDs are **allocation-stable**: assigned from a
  persisted counter at `CREATE TYPE`, kept across `ALTER TYPE … ADD VALUE`,
  and never renumbered or reused after `DROP TYPE` — the previous positional
  mint (base + sorted-name index) shifted every enum's OID whenever a
  lexically-earlier type appeared, which would send a client's registered
  loader decoding the wrong type.
- `executor._out_column_descs`: enum-aware `(name, Column)` → `ColumnDesc`
  resolution, used by SELECT (plain, correlated), INSERT / UPDATE / DELETE /
  MERGE `RETURNING`, and extended-protocol Describe (statements and
  RETURNING).
- `virtual._pg_type`: enum / domain / composite rows carry a derived
  `typarray` (`oid + USER_TYPE_ARRAY_OID_OFFSET`) instead of 0.
- `scalar.py` / `planner.py`: casts to a declared enum (`'ok'::mood`,
  `%s::mood`, `%s::mood[]`) validate labels (`22P02`) and describe with the
  enum's OID (arrays: the paired array OID) in constant selects.
- `pgextended.py`: a Bind parameter declared with an enum OID is
  label-validated (`22P02`); binary array parameters and results handle
  user-type array OIDs (elements travel as text — an enum's wire form is its
  label).

#### Fixed

- `executor.py` / `engine.py`: enum columns in `RowDescription` report the
  enum's OID instead of 25 across the simple-SELECT, RETURNING, and Describe
  paths. JOIN / GROUP BY / evaluated-expression plans still describe enum
  outputs as `text` (their column shape drops the enum tag at plan time) —
  recorded in `tasks/backlog.md`.
- `scalar.py`: `oid::regtype::text` of a user type quotes names that need it
  (`"CamelCaseEnum"`) — an unquoted mixed-case name pasted back as a cast
  suffix folds to lowercase and misses the type.
- `virtual.user_type_oid`: `::regtype` / `to_regtype()` resolution of a
  user-declared type name now applies Postgres identifier folding — an
  unquoted part folds to lowercase (`'StrTestEnum'::regtype` finds
  `strtestenum`), a quoted part keeps its case. psycopg's
  `EnumInfo.fetch(conn, "MixedCaseName")` was returning `None`, which
  poisoned its entire enum-adaptation suite.
