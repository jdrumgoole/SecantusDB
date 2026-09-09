### The Rust PostgreSQL server enforces NOT NULL, CHECK and FOREIGN KEY

`CREATE TABLE` on the Rust PostgreSQL server now records its NOT NULL, CHECK
and FOREIGN KEY constraints in the shared catalog and enforces them on every
INSERT, UPDATE and DELETE, answering what PostgreSQL 16 answers: `23502` with
the failing column, `23514` with the constraint's name, `23503` on the child
side and — for NO ACTION / CASCADE / SET NULL — the parent side, each with the
`Failing row contains (...)` / `Key (...)=(...)` detail and the schema, table,
column and constraint diagnostic fields a driver reads. A `DEFERRABLE
INITIALLY DEFERRED` key is checked at COMMIT: the COMMIT reports the
violation, the transaction rolls back and the connection is left idle, as it
is on PostgreSQL. Unnamed constraints take PostgreSQL's generated names
(`<table>_<column>_check`, `<table>_check1`, `<table>_<column>_fkey`), and a
CHECK naming a missing column or a foreign key without a unique target is
refused at CREATE (`42703`, `42830`).

#### Added

- Rust PostgreSQL server: NOT NULL (`23502`), CHECK (`23514`) and FOREIGN KEY
  (`23503`, immediate and `INITIALLY DEFERRED` to COMMIT) enforcement with
  PostgreSQL's messages, details and diagnostic fields; `ON DELETE CASCADE` /
  `SET NULL`; temp tables report a `pg_temp` schema in diagnostics.
- `secantus-pgcatalog`: `TableDef` carries `temp`, `check_constraints` and
  `foreign_keys` in the Python server's document shape.

- Rust PostgreSQL server: `CancelRequest` interrupts the running statement
  (`57014 canceling statement due to user request`, connection left idle);
  `pg_stat_activity` shows each backend's state and running query; and
  `idle_in_transaction_session_timeout` / `idle_session_timeout` are
  validated, rendered (`60000` shows as `1min`) and enforced — the session
  ends with FATAL `25P03` / `57P05` and the connection closes, as on
  PostgreSQL 16.
- Rust PostgreSQL server: databases. The startup packet's `dbname` is checked
  before `AuthenticationOk` — an unknown name is FATAL `3D000 database "x"
  does not exist` (a failed connect in libpq, not a failed first query) and
  `template0` is `55000` — against a registry of `postgres` / `template1` /
  `template0`, the daemon's `--database NAME` flags and `CREATE DATABASE`;
  `DROP DATABASE` (with PostgreSQL's `25001`, `42P04`, `3D000` / `IF EXISTS`
  notice, `55006` and `42809`) drops the data too; `pg_database` lists the
  set and `current_database()` / `current_catalog` name the connected one.

- Rust PostgreSQL server: `GROUP BY` over an expression (`length(data)`,
  `col is null`, `n + 1`) or a select-list position (`GROUP BY 1, 2`), with
  the key matched to the projected expression by structure and to `ORDER BY`
  by position, alias or expression; `IS [NOT] NULL` as a value, including
  PostgreSQL's row rule (`row(1, null)` is neither null nor not null); and a
  FROM-less `select unnest(array)` as one row per element in a column named
  `unnest` of the element type.
- Rust PostgreSQL server: a table is also its row type, as on PostgreSQL —
  `CREATE TABLE rtt (...)` registers the composite `rtt`, so `'(1,foo)'::rtt`,
  `'{"(1,foo)"}'::rtt[]`, `row(1,'x')::rtt`, `pg_typeof`, `to_regtype('rtt')`
  and the `pg_type` / `pg_attribute` rows psycopg's `TypeInfo.fetch` reads all
  see it; `DROP TABLE` removes it; `DROP TYPE rtt` is `2BP01 cannot drop type
  rtt because table rtt requires it` with the `You can drop table rtt
  instead.` hint; and a `CREATE TYPE` / `CREATE TABLE` over an existing type
  or relation is `42710 type "x" already exists` (with PostgreSQL's hint when
  a relation collides with a type) or `42P07 relation "x" already exists`.
  `to_regtype` also resolves a user type's array — `mood[]`, `rtt[]` or the
  internal `_rtt` spelling — which was NULL for every enum and composite.
- Rust PostgreSQL server: the `aclitem` type (oid 1033, array 1034) with
  PostgreSQL 16's parser and renderer — `grantee=privileges/grantor` with
  the `group` / `user` key words, quoted names, `*` grant options and the
  canonical `arwdDxtXUCTcsA` order — and its errors (`role "x" does not
  exist`, `invalid mode character`, `unrecognized key word` with its hint,
  `extra garbage at the end of the ACL specification`). The roles it knows
  are the session user (there is no role catalog); an omitted grantor
  defaults to it with PostgreSQL's `defaulting grantor to user ID 10`
  WARNING, and a planner WARNING now reaches the client as a
  NoticeResponse.
- Rust PostgreSQL server: `SET standard_conforming_strings TO off` is
  honoured and reported. A plain `'...'` literal is then read with the
  pre-9.1 backslash escapes (`'a\'b'`, `'p\nq'`, `'\\'`), `E'...'`,
  `B'...'`, `X'...'` and `$$...$$` keep their own rules, a statement is read
  under the setting in force when it is prepared, and `U&'...'` is refused
  as PostgreSQL refuses it (`0A000 unsafe use of string constant with
  Unicode escapes`). `escape_string_warning` (default on) raises PostgreSQL's
  `22P06 nonstandard use of \' / \\ / escape in a string literal` WARNING
  per literal, with its hint and position. Both GUCs are validated as
  Booleans in every spelling PostgreSQL accepts (`22023 parameter "x"
  requires a Boolean value`), and the `standard_conforming_strings` value is
  sent as a `ParameterStatus` — which is what libpq's `PQescapeString`
  follows — because the server now obeys it.

#### Fixed

- Rust PostgreSQL server: a long statement no longer stalls every other
  connection — execution runs off the async runtime's I/O thread, so a
  cancel request (or any other client) is served while `pg_sleep` runs.

- Rust PostgreSQL server: every extended-protocol statement between two
  `Sync`s runs in one transaction that the `Sync` commits, as on PostgreSQL —
  an error in a pipeline now rolls back the earlier statements of its group
  (libpq's `PIPELINE_ABORTED` batch is all-or-nothing), `BEGIN` inside a
  group turns it into a block, and `DECLARE` in a group is still `25P01`.
- Rust PostgreSQL server: a statement prepared without parameter types
  (libpq `PQprepare` with `nParams = 0`) sizes its parameters from the lexer,
  so `insert into t values ($1, $2)` no longer fails with `there is no
  parameter $1` — pg_query's node walk skips a VALUES list.
- Rust PostgreSQL server: the first DDL on a fresh store no longer fails a
  second connection with a WiredTiger `WriteConflict`. The `__sql_*` catalog
  collections were registered lazily inside whichever block first needed one,
  and a block that began before the row landed could not see it; they are
  now created before a transaction handle opens. The enum / composite type
  oid counter is likewise advanced outside the block, like PostgreSQL's OID
  counter, so two open `CREATE TYPE` blocks no longer conflict (a rolled-back
  block just skips an oid).
- Rust PostgreSQL server: an assignment with no assignment cast is refused
  as PostgreSQL refuses it — a `text` / `varchar`-typed expression (an
  explicit cast, or a parameter the client declared, which is how psycopg
  sends a binary-format string) into a `jsonb`, `integer`, `date`, ... column,
  or `boolean` into an integer column, is `42804 column "data" is of type
  jsonb but expression is of type text` with the `You will need to rewrite
  or cast the expression.` hint, on INSERT and UPDATE. Before this the value
  was coerced through the column's parser and stored.
- Rust PostgreSQL server: the `TimeZone` / `DateStyle` `ParameterStatus` sent
  at startup now carries the session's values (`UTC`, `ISO, MDY`) rather than
  the wire library's defaults, so what a client caches at connect is what
  `SHOW` reports.
- Rust PostgreSQL server: a record literal of the wrong width reports
  PostgreSQL's message and detail — `22P02 malformed record literal: "(1)"`
  with `Too few columns.` / `Too many columns.`, and `row(1)::rtt` is `42846
  cannot cast type record to rtt` with `Input has too few columns.` /
  `Input has too many columns.`; a planner error's `Detail:` line now travels
  in the error's detail field rather than its message.
- Rust PostgreSQL server: a syntax error's message is PostgreSQL's alone —
  `syntax error at or near "selct"`, `unterminated quoted string at or near
  "'q"` — without the `Error splitting: ` / `Invalid statement: ` label the
  libpg_query Rust binding prefixes it with, which reached the client's
  `message_primary` on every syntax error.
