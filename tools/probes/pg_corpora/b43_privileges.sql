# reference-version: 15
# Batch 43: an unqualified name skips a search_path schema the role cannot
# use; schema CREATE, sequence USAGE / SELECT / UPDATE at nextval / currval /
# setval (and SELECT on the sequence as a relation) and function EXECUTE at
# a call are enforced; a function's grants are per signature; GRANT ON ALL
# SEQUENCES / FUNCTIONS IN SCHEMA expands to what exists.
REVOKE EXECUTE ON FUNCTION b43f(int) FROM PUBLIC
SET ROLE b43r
SET search_path = b43s, public
SELECT * FROM only_s
SELECT * FROM t
SELECT count(*) FROM b43s.t
SELECT current_schemas(false)
CREATE TABLE b43s.x (a int)
SELECT nextval('b43q')
SELECT currval('b43q')
SELECT setval('b43q', 5)
SELECT last_value FROM b43q
SELECT b43f(1)
SELECT b43f('a'::text)
SELECT has_function_privilege('b43f(int)', 'EXECUTE'), has_function_privilege('b43f(text)', 'EXECUTE')
RESET ROLE
RESET search_path
GRANT USAGE ON SCHEMA b43s TO b43r
SET ROLE b43r
SET search_path = b43s, public
SELECT * FROM only_s
SELECT current_schemas(false)
CREATE TABLE x (a int)
CREATE TABLE b43s.x (a int)
CREATE SEQUENCE b43s.qx
CREATE VIEW b43s.v AS SELECT 1 AS a
CREATE TEMP TABLE b43tmp (a int)
RESET ROLE
RESET search_path
GRANT CREATE ON SCHEMA b43s TO b43r
SET ROLE b43r
CREATE TABLE b43s.x (a int)
DROP TABLE b43s.x
CREATE TABLE b43pub (a int)
RESET ROLE
GRANT USAGE ON SEQUENCE b43q TO b43r
SET ROLE b43r
SELECT nextval('b43q')
SELECT currval('b43q')
SELECT setval('b43q', 5)
SELECT last_value FROM b43q
RESET ROLE
REVOKE USAGE ON SEQUENCE b43q FROM b43r
GRANT SELECT ON SEQUENCE b43q TO b43r
SET ROLE b43r
SELECT nextval('b43q')
SELECT currval('b43q')
SELECT last_value FROM b43q
RESET ROLE
REVOKE SELECT ON SEQUENCE b43q FROM b43r
GRANT UPDATE ON SEQUENCE b43q TO b43r
SET ROLE b43r
SELECT nextval('b43q')
SELECT setval('b43q', 7)
SELECT currval('b43q')
RESET ROLE
SELECT has_sequence_privilege('b43r', 'b43s.q2', 'USAGE')
GRANT USAGE ON ALL SEQUENCES IN SCHEMA b43s TO b43r
SELECT has_sequence_privilege('b43r', 'b43s.q2', 'USAGE'), has_sequence_privilege('b43r', 'b43s.q2', 'SELECT')
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA b43fs FROM PUBLIC
SELECT has_function_privilege('b43r', 'b43fs.g()', 'EXECUTE')
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA b43fs TO b43r
SELECT has_function_privilege('b43r', 'b43fs.g()', 'EXECUTE')
GRANT USAGE ON ALL SEQUENCES IN SCHEMA b43nosuch TO b43r
GRANT EXECUTE ON FUNCTION b43f TO PUBLIC
GRANT EXECUTE ON FUNCTION b43nosuchf(int) TO PUBLIC
GRANT EXECUTE ON FUNCTION b43nosuchf TO PUBLIC
SELECT has_function_privilege('b43nosuchf()', 'EXECUTE')
SELECT has_function_privilege('b43f', 'EXECUTE')
GRANT EXECUTE ON FUNCTION b43f(int) TO b43r
SELECT has_function_privilege('b43r', 'b43f(int)', 'EXECUTE'), has_function_privilege('b43r', 'b43f(integer)', 'EXECUTE')
DROP FUNCTION b43f(int)
CREATE FUNCTION b43f(int) RETURNS int LANGUAGE sql AS 'select 1'
SELECT has_function_privilege('b43r', 'b43f(int)', 'EXECUTE')
REVOKE ALL ON SEQUENCE b43q FROM b43r
DROP SEQUENCE b43q
CREATE SEQUENCE b43q
GRANT USAGE ON SEQUENCE b43q TO b43r
DROP SEQUENCE b43q
CREATE SEQUENCE b43q
SELECT has_sequence_privilege('b43r', 'b43q', 'USAGE')
DROP TABLE IF EXISTS b43pub
DROP SCHEMA b43s CASCADE
DROP SCHEMA b43fs CASCADE
DROP FUNCTION b43f(int)
DROP FUNCTION b43f(text)
DROP SEQUENCE b43q
DROP TABLE IF EXISTS b43tmp
DROP ROLE b43r
