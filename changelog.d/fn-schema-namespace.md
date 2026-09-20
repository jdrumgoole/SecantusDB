### A function created in a schema was invisible in it

`CREATE FUNCTION hasfunctions.addfunction(...)` stored the function and then
reported it in `public`. Only a `pg_temp_` qualifier was preserved at creation;
every other schema was dropped. The function existed, worked, and could not be
found by any client that filters by schema — which pgjdbc's `getFunctions` and
`getProcedures` both do.

Underneath that were two more defects on the same rows. `pg_proc.proname`
returned the dotted storage key rather than the bare name, and `prokind` was
hardcoded `'f'` — so **every procedure was reported as a function**, and
`getProcedures()`, which filters on `prokind = 'p'`, returned nothing at all
regardless of schema.

Fixing only the catalog half would have made things worse: the routine is
stored under a dotted key, so recording the schema without teaching the call
path about it made `hf.addf(1, 2)` raise "function addf does not exist" — the
namespace correct and the function unusable. A qualified call now resolves
against the dotted key first, then the bare name, so builtins
(`pg_catalog.now()`) and unqualified calls are unaffected.

#### Fixed

- A schema qualifier is preserved for any schema, not just `pg_temp_`.
  `public` stays unqualified: it is the default search_path schema, so its
  functions must keep resolving from a bare name.
- `pg_proc.pronamespace` resolves to the routine's real schema, and `proname`
  is the bare name.
- `pg_proc.prokind` is `'p'` for a procedure.
- A schema-qualified call resolves the dotted key.

#### Changed

- A **bare** call to a function homed in a non-search_path schema now raises
  `function addf(integer, integer) does not exist`. This server used to answer
  it; PostgreSQL 14 raises exactly that error (measured 2026-09-19), so the
  new behaviour is the faithful one.

Measured on pgjdbc's `DatabaseMetaDataTest` (194 tests): **36 failures → 34**,
23 distinct → 21, no regressions. `getFunctionsInSchemaForFunctions` and
`getProceduresInSchemaForProcedures` go green.
