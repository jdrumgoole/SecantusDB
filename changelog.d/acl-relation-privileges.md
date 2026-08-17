### getTablePrivileges reflects relation ownership and ACLs

pgjdbc's getTablePrivileges (and getViewPrivileges / matview variants)
returned nothing: `pg_class.relowner` was hardcoded to PG's
bootstrap-superuser oid 10, so the driver's `c.relowner = r.oid` join
against the minted role oid found no rows. relowner now resolves to the
connecting user's role oid (every relation is owned by its creator), and
`pg_class.relacl` reflects the relation's ACL — NULL while untouched (a
driver reads that as the owner holding every privilege implicitly), and
a materialized aclitem array once a GRANT/REVOKE touches it. `REVOKE ALL
… FROM <owner>` empties the owner's entry, so getTablePrivileges then
reports no rows (keeping noTablePrivileges correct).

#### Fixed
- `pg_class.relowner` on tables / views / materialized views resolves to
  the owning role's oid (was 10), so getTablePrivileges' owner join
  works.
- `pg_class.relacl` materializes from recorded grants (owner-implicit
  privileges + per-grantee grants; `REVOKE ALL FROM owner` → `{}`).
