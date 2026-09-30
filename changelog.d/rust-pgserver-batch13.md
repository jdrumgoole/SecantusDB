### The Rust PostgreSQL server: procedures, ALTER FUNCTION, column privileges, every RENAME, and PostgreSQL's own catalogs

Batch 13 continued from the re-measured backlog. Two of its findings were
security bugs, and both are fixed: column privileges acted as table privileges,
and dropped tables' grants survived. It also fills in a family of missing
statements: procedures, `ALTER FUNCTION` and the `RENAME` forms. Every change
is measured against PostgreSQL 15.

#### Fixed

- **A column-level GRANT acted as a table-level one.** After `GRANT SELECT (v)
  ON t TO r`, the grantee could read every column of `t`. Column privileges
  are now recorded in the shared `__sql_column_grants__` catalog (the Python
  server's shape) and checked against the columns a statement uses.
- **Grants outlived their table.** A table created under a dropped table's
  name inherited its grants. `DROP TABLE` / `DROP VIEW` now drop them, a
  rename moves them, and a dropped column's grants go with it.
- **A procedure could be called with SELECT**, which ran it as a function.
  That is now `42809 ... is a procedure`, as on PostgreSQL.
- **A PRIMARY KEY or UNIQUE constraint over a nondeterministic collation**
  accepted `apple` beside `Apple`. Keys are now compared by the collation's
  sort key, and `count(DISTINCT ...)` counts them the same way.
- A named `CONSTRAINT ... PRIMARY KEY` was reported as `<table>_pkey`.
- `relacl` lost the owner's entry once every grant was revoked. `relacl` and
  `attacl` are now `aclitem[]`, and `attacl` shows column grants.
- A function with OUT parameters returned NULL (plpgsql) or a single record
  column from `select * from f()`.
- A `void` function's value is now `''`, not NULL.

#### Added

- **`CREATE / CALL / DROP PROCEDURE`**, with OUT and INOUT parameters.
- **`ALTER FUNCTION / PROCEDURE / ROUTINE`**: RENAME, SET SCHEMA, OWNER TO,
  volatility, STRICT, SECURITY, LEAKPROOF, COST, ROWS, PARALLEL, and SET /
  RESET.
- **`CREATE FUNCTION` without RETURNS** (the result comes from the OUT
  parameters), and function schemas. Functions are stored in the shared
  shape, every parameter with its mode.
- **`ALTER ... RENAME`** for indexes, constraints, sequences, types (with enum
  values and composite attributes), domains (and their constraints), schemas,
  triggers and rules. Each rewrites every stored reference to the object.
- **Rules**: a rule action with several VALUES rows, `DEFAULT` in VALUES, and
  `CREATE RULE "_RETURN" ... ON SELECT` turning an empty table into a view. A
  set-operation action is refused at CREATE RULE (`42P10`).
- **Event triggers**:
  - `table_rewrite` fires, with `pg_event_trigger_table_rewrite_oid()` and
    `_reason()`.
  - `pg_event_trigger_ddl_commands()` covers schemas, comments, renames,
    grants, routines, domains, sequences, types, triggers, rules, policies,
    statistics and publications, and has a `command` column.
  - The event functions raise `39P03` outside their event.
- **Catalogs**:
  - `pg_class` and `pg_attribute` list PostgreSQL 15's own relations and their
    columns, and those relations resolve through `::regclass`.
  - `pg_type` has a row type for each view and an array type for every user
    type, and `pg_rewrite` lists each view's `_RETURN` rule.
  - `pg_proc` has argument modes, namespaces and owners.
  - `pg_indexes` prints `COLLATE` and ruleutils' parenthesised expressions.
- Corpora `column_privileges`, `rule_shapes`, `nondeterministic_keys`,
  `procedures`, `alter_routines`, `event_trigger_coverage`, `renames` and
  `catalog_system`, all at 0 divergences against PostgreSQL 15.
