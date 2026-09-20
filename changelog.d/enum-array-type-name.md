### An array-of-enum column reported the enum, not the array type

A column declared `test_schema.test_enum[]` recorded the **element** enum's oid.
`pg_attribute`'s enum branch had no array handling — the composite branch beside
it already did — so a client resolving the column found a `typtype = 'e'` enum
where PostgreSQL 14 reports the `typtype = 'b'` array type. pgjdbc's
`getColumns` returned TYPE_NAME `"test_schema"."test_enum"` and the element's
DATA_TYPE instead of `"test_schema"."_test_enum"` and `ARRAY`.

Fixing that exposed a second defect underneath it. Array type names were made
unique against a **single global set** of every type name in the catalog, but a
PostgreSQL type name is unique only within its own namespace. So
`test_schema.test_enum`'s array dodged the entirely unrelated
`public._test_enum` and came out `__test_enum`, and the miscount compounded —
an array in `public` whose name was already taken landed on four underscores
where PostgreSQL uses three.

Both fixed, and every name was measured against PostgreSQL 14 on 2026-09-20
rather than reasoned about — including the three-underscore case, which looks
like an off-by-one and is not: `_test_enum` is taken by an enum of that literal
name, and `__test_enum` by *that* enum's own array, which is created first
because it has the lower oid.

#### Fixed

- An array-of-enum column reports the array type's oid.
- Array type-name collisions are resolved per namespace.

Measured on pgjdbc's `DatabaseMetaDataTest` (194 tests): **33 failures → 31**,
20 distinct → 18, no regressions. `getCorrectSQLTypeForOffPathTypes` and
`getCorrectSQLTypeForShadowedTypes` go green.
