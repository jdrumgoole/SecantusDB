### A dropped table's privileges came back with the next table of that name

Dropping a table removed its definition and left everything that governed
access to it behind: the table's grants, its column grants, its `relacl` state
and its RLS policies. Creating a table under the same name silently reattached
all four — **including grants to other roles**.

That is a privilege-escalation shape rather than a metadata nit. Dropping a
table is how someone revokes all access to it; before this, the replacement
table still granted what the old one did.

Measured against PostgreSQL 14 on 2026-09-20: after `DROP TABLE` + `CREATE
TABLE` the new table reports `relacl` NULL and has no grants, column grants or
policies. Comments and the RLS-enabled flag were already cleared correctly; the
other four were not.

#### Fixed

- `DROP TABLE` clears the table's grants, column grants, `relacl` state and RLS
  policies along with its definition.

Found from pgjdbc's `DatabaseMetaDataTest::tablePrivileges`, which passed or
failed depending on run order: the class's `noTablePrivileges` test revokes
everything on `metadatatest`, and the fixture drops and recreates that table
between every test, so on a real server the revoke cannot carry over. The
initial diagnosis — "a view's privileges resolve and a table's do not" — was
wrong, and checking it rather than building on it is what led here: the same
test was passing in one parameterised run and failing in the other.

Measured on pgjdbc's `DatabaseMetaDataTest` (194 tests): **31 failures → 30**,
18 distinct → 17, no regressions. `tablePrivileges` goes green. The single-test
delta understates the change; the leak it exposed is the substance.

A full-suite run here also surfaced a parallel-worker race in
`test_pg_numeric_wide_grouping.py` (every xdist worker created a table named
`t` in the shared reference PostgreSQL's `public` schema, so workers raced on
drop/create). That turned out to be fixed independently in #1536 while this
branch was in flight, so nothing for it is carried here — #1536's per-case
schema is what ships.
