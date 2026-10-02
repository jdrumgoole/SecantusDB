# reference-version: 15
# has_table_privilege answers from the GRANTs, owners and role membership
# (it answered true for every role and table).
GRANT SELECT ON b41a.t TO b41r
GRANT b41grp TO b41r
GRANT INSERT ON b41x TO b41grp
GRANT UPDATE ON b41x TO b41r WITH GRANT OPTION
GRANT SELECT ON b41vv TO PUBLIC
SELECT has_table_privilege('b41r', 'b41a.t', 'SELECT'), has_table_privilege('b41r', 'b41b.t', 'SELECT')
SELECT has_table_privilege('b41r', 'b41x', 'SELECT'), has_table_privilege('b41r', 'b41x', 'INSERT')
SELECT has_table_privilege('b41r', 'b41x', 'select, insert'), has_table_privilege('b41r', 'b41x', 'DELETE, TRUNCATE')
SELECT has_table_privilege('b41r', 'b41x', 'UPDATE WITH GRANT OPTION'), has_table_privilege('b41r', 'b41x', 'INSERT WITH GRANT OPTION')
SELECT has_table_privilege('b41r', 'b41vv', 'SELECT'), has_table_privilege('b41r', 'b41vv', 'INSERT')
SELECT has_table_privilege('b41r', 'pg_class', 'SELECT')
SELECT has_table_privilege('b41x', 'SELECT'), has_table_privilege('b41a.t', 'DELETE')
SELECT has_table_privilege('b41r', 'b41x'::regclass, 'INSERT')
SELECT has_table_privilege('b41r', 'b41x'::regclass::oid, 'DELETE')
SELECT has_table_privilege('b41nosuchrole', 'b41x', 'SELECT')
SELECT has_table_privilege('b41r', 'b41nosuch', 'SELECT')
SELECT has_table_privilege('b41r', 'b41x', 'FROB')
SELECT has_table_privilege('pg_class', 'Frob with grant option')
SELECT has_table_privilege('pg_class', 'select, Frob ')
SELECT relname FROM pg_class WHERE relname IN ('b41x', 'b41vv') AND has_table_privilege('b41r', oid, 'INSERT') ORDER BY 1
SET ROLE b41r
SELECT has_table_privilege('b41x', 'INSERT'), has_table_privilege('b41x', 'DELETE')
RESET ROLE
REVOKE b41grp FROM b41r
SELECT has_table_privilege('b41r', 'b41x', 'INSERT')
DROP SCHEMA b41a CASCADE
DROP SCHEMA b41b CASCADE
DROP VIEW b41vv
DROP TABLE b41x
DROP ROLE b41r
DROP ROLE b41grp
