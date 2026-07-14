### SQL: psycopg TypeInfo catalog fidelity (typarray, pg_range, to_regtype)

psycopg's type-registration machinery works end-to-end: `TypeInfo.fetch`,
`RangeInfo.fetch`, `MultirangeInfo.fetch`, `EnumInfo.fetch` (with labels),
and `CompositeInfo.fetch` (with field names) all resolve against the virtual
catalog. `pg_type` gains `typarray` / `typdelim`, a `pg_range` table maps
range oids to their declared subtype and multirange oids, `to_regtype()` is
implemented (built-ins and user-declared enum/domain/composite types,
returning NULL for unknown names), and `oid::regtype::text` renders
user-declared type names. Catalog-table WHEREs that can't lower now evaluate
per-row with the real catalog in scope, and a context-dependent function call
(`to_regtype('mood')`) is no longer folded as if it were a NULL literal.

#### Added

- `pg_type.typarray` / `typdelim` columns; the `pg_catalog.pg_range` virtual
  table (`rngtypid` / `rngsubtype` / `rngmultitypid`, declared subtypes —
  `tsrange` advertises `timestamp`, `daterange` advertises `date`);
  `to_regtype(name)` (scalar + FROM-less + pushdown-constant paths).

#### Fixed

- `oid::regtype` on a user-declared type's oid resolves its name through the
  catalog instead of raising 42704.
- The catalog-table fast path publishes the planning subquery context and
  routes non-lowerable WHEREs through per-row evaluation with the real
  catalog (a synthetic catalog over the row backend knew no user types).
- The NULL-operand comparison folds no longer treat an `Anonymous` function
  call as a NULL literal (`WHERE t.oid = to_regtype('mood')` matched nothing).
