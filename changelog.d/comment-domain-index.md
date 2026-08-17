### COMMENT ON DOMAIN / INDEX and obj_description()

Two more COMMENT targets and the lookup function that reads them.
`COMMENT ON DOMAIN` stores on the domain (surfacing in `pg_description`
under classoid `pg_type`), `COMMENT ON INDEX` stores by index name
(resolved to the index relation's oid at read time), and
`obj_description(oid[, 'catalog'])` / `col_description(oid, attnum)`
look comments up the way pgjdbc's getUDTs and getIndexInfo do. `IS
NULL` removes a comment on both new targets.

#### Added
- `COMMENT ON DOMAIN d IS '…' | NULL` (sqlglot Command fallback path).
- `COMMENT ON INDEX i IS '…' | NULL` with pg_description reflection.
- `obj_description` / `col_description` scalar functions.
