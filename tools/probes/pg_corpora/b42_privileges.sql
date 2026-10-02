# reference-version: 15
# Schema USAGE is enforced; has_schema / _column / _sequence / _function
# _privilege answer from GRANTs and owners; information_schema.role_table_grants
# and table_privileges exist.
GRANT SELECT ON b42s.t TO b42r
SET ROLE b42r
SELECT * FROM b42s.t
RESET ROLE
SELECT has_schema_privilege('b42r', 'b42s', 'USAGE'), has_schema_privilege('b42r', 'public', 'USAGE'), has_schema_privilege('b42r', 'public', 'CREATE')
SELECT has_schema_privilege('b42s', 'USAGE, CREATE'), has_schema_privilege('b42r', 'b42s', 'CREATE')
SELECT has_schema_privilege('b42r', 'b42nosuch', 'USAGE')
SELECT has_schema_privilege('b42r', 'b42s', 'SELECT')
GRANT USAGE ON SCHEMA b42s TO b42g
GRANT b42g TO b42r
SELECT has_schema_privilege('b42r', 'b42s', 'USAGE')
SET ROLE b42r
SELECT * FROM b42s.t
INSERT INTO b42s.t VALUES (3, 4)
RESET ROLE
REVOKE b42g FROM b42r
GRANT USAGE ON SCHEMA b42s TO b42r WITH GRANT OPTION
SELECT has_schema_privilege('b42r', 'b42s', 'USAGE WITH GRANT OPTION'), has_schema_privilege('b42r', 'b42s', 'CREATE WITH GRANT OPTION')
REVOKE USAGE ON SCHEMA b42s FROM b42r
SET ROLE b42r
SELECT * FROM b42s.t
RESET ROLE
GRANT FROB ON SCHEMA b42s TO b42r
GRANT USAGE ON SCHEMA b42nosuch TO b42r
SELECT has_column_privilege('b42r', 'b42s.t', 'a', 'SELECT'), has_column_privilege('b42r', 'b42s.t', 'a', 'UPDATE')
GRANT UPDATE (b) ON b42t TO b42r
SELECT has_column_privilege('b42r', 'b42t', 'b', 'UPDATE'), has_column_privilege('b42r', 'b42t', 'a', 'UPDATE'), has_column_privilege('b42r', 'b42t', 'b', 'INSERT')
SELECT has_column_privilege('b42r', 'b42t', 'b', 'SELECT, UPDATE'), has_column_privilege('b42t', 'a', 'INSERT')
SELECT has_column_privilege('b42r', 'b42t', 'nosuch', 'SELECT')
SELECT has_column_privilege('b42r', 'pg_class', 'relname', 'SELECT')
SELECT has_sequence_privilege('b42r', 'b42q', 'USAGE'), has_sequence_privilege('b42r', 'b42s.q', 'USAGE')
GRANT USAGE ON SEQUENCE b42q TO b42r
GRANT SELECT, UPDATE ON SEQUENCE b42s.q TO b42r
SELECT has_sequence_privilege('b42r', 'b42q', 'USAGE'), has_sequence_privilege('b42r', 'b42q', 'SELECT')
SELECT has_sequence_privilege('b42r', 'b42s.q', 'UPDATE'), has_sequence_privilege('b42r', 'b42s.q', 'USAGE')
SELECT has_sequence_privilege('b42q', 'USAGE, SELECT, UPDATE')
SELECT has_sequence_privilege('b42r', 'b42t', 'USAGE')
SELECT has_sequence_privilege('b42r', 'b42q', 'EXECUTE')
REVOKE ALL ON SEQUENCE b42q FROM b42r
SELECT has_sequence_privilege('b42r', 'b42q', 'USAGE')
SELECT has_function_privilege('b42r', 'b42f()', 'EXECUTE')
REVOKE EXECUTE ON FUNCTION b42f() FROM PUBLIC
SELECT has_function_privilege('b42r', 'b42f()', 'EXECUTE'), has_function_privilege('b42f()', 'EXECUTE')
GRANT EXECUTE ON FUNCTION b42f() TO b42r
SELECT has_function_privilege('b42r', 'b42f()', 'EXECUTE')
SELECT has_function_privilege('b42r', 'b42f()', 'USAGE')
SELECT grantor = current_user, CASE WHEN grantee = current_user THEN 'me' ELSE grantee END, table_schema, table_name, privilege_type, is_grantable, with_hierarchy FROM information_schema.role_table_grants WHERE table_name LIKE 'b42%' OR table_schema = 'b42s' ORDER BY 2, 3, 4, 5
GRANT INSERT ON b42t TO PUBLIC
SELECT grantor = current_user, CASE WHEN grantee = current_user THEN 'me' ELSE grantee END, table_name, privilege_type FROM information_schema.table_privileges WHERE table_name = 'b42t' ORDER BY 2, 4
SELECT count(*) FROM information_schema.role_table_grants WHERE table_name = 'b42t' AND grantee = 'PUBLIC'
SET ROLE b42r
SELECT grantee, table_name, privilege_type FROM information_schema.role_table_grants WHERE table_name LIKE 'b42%' ORDER BY 1, 2, 3
RESET ROLE
DROP SCHEMA b42s CASCADE
DROP TABLE b42t
DROP SEQUENCE b42q
DROP FUNCTION b42f()
DROP ROLE b42r
DROP ROLE b42g
