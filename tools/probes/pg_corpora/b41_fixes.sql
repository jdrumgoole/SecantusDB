# reference-version: 15
# A postmaster / sighup / backend parameter cannot be SET by a session
# (55P02), by SET, set_config or ALTER DATABASE; SHOW still reads the default.
SET shared_buffers = '256MB'
SET max_connections = 5
SET archive_command = 'x'
SET log_checkpoints = on
SET log_connections = on
SELECT set_config('shared_buffers', '1MB', false)
SHOW shared_buffers
SET work_mem = '8MB'
RESET work_mem
# A pg_temp function is reached only by a pg_temp-qualified call.
CREATE FUNCTION pg_temp.b41_tf() RETURNS int LANGUAGE sql AS 'select 41'
SELECT b41_tf()
SELECT pg_temp.b41_tf()
DROP FUNCTION pg_temp.b41_tf()
# No min / max over these types in PostgreSQL 15 (42883); inet and pg_lsn have them.
SELECT min('08:00:2b:01:02:03'::macaddr)
SELECT max(x) FROM (VALUES ('08:00:2b:01:02:03'::macaddr8)) v(x)
SELECT max(x) FROM (VALUES ('a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid)) v(x)
SELECT min(x) FROM (VALUES ('\x01'::bytea)) v(x)
SELECT max(x) FROM (VALUES ('{}'::jsonb)) v(x)
SELECT min(x) FROM (VALUES (B'101')) v(x)
SELECT max(x) FROM (VALUES ('1.2.3.4'::inet), ('1.2.3.5'::inet)) v(x)
SELECT max(x) FROM (VALUES ('1/2'::pg_lsn), ('1/3'::pg_lsn)) v(x)
SELECT max(x) FROM (VALUES (ARRAY[true]), (ARRAY[false])) v(x)
# A regclass text walks the search path.
SET search_path = b41a, public
SELECT 't'::regclass::text
SELECT 't'::regclass = 'b41a.t'::regclass
SELECT to_regclass('t')::text
SELECT relname FROM pg_class WHERE oid = 't'::regclass
SELECT nextval('q')
SELECT 'b41b.t'::regclass::text
RESET search_path
SELECT 'b41a.t'::regclass::text
SELECT to_regclass('t')
# Index comments are per schema.
COMMENT ON INDEX b41a.b41ix IS 'in a'
COMMENT ON INDEX b41b.b41ix IS 'in b'
SELECT obj_description('b41a.b41ix'::regclass, 'pg_class'), obj_description('b41b.b41ix'::regclass, 'pg_class')
# A view over another schema's tables prints them qualified, unaliased.
CREATE VIEW b41a.v AS SELECT a, b FROM b41a.t WHERE a > 1
SELECT pg_get_viewdef('b41a.v'::regclass)
CREATE VIEW b41a.v2 AS SELECT t.a FROM b41a.t JOIN b41b.t u ON u.a = t.a
SELECT pg_get_viewdef('b41a.v2'::regclass)
CREATE VIEW b41a.v3 AS SELECT x.a FROM b41a.t x
SELECT pg_get_viewdef('b41a.v3'::regclass)
# A privilege is checked on the table a schema-qualified name reads.
GRANT USAGE ON SCHEMA b41a TO b41r
GRANT USAGE ON SCHEMA b41b TO b41r
GRANT SELECT ON b41a.t TO b41r
SET ROLE b41r
SELECT count(*) FROM b41a.t
SELECT count(*) FROM b41b.t
INSERT INTO b41b.t VALUES (1)
UPDATE b41a.t SET a = 1
RESET ROLE
# A FILTER holding a subquery, for the aggregates that keep NULL inputs.
SELECT array_agg(a) FILTER (WHERE a IN (SELECT k FROM b41g)) FROM b41f
SELECT g, array_agg(a ORDER BY a) FILTER (WHERE a IN (SELECT k FROM b41g)) FROM b41f GROUP BY g ORDER BY g
SELECT g, json_agg(s) FILTER (WHERE EXISTS (SELECT 1 FROM b41g WHERE k = 5)) FROM b41f GROUP BY g ORDER BY g
SELECT jsonb_agg(a) FILTER (WHERE a > (SELECT min(k) FROM b41g)) FROM b41f
SELECT array_agg(s) FILTER (WHERE a NOT IN (SELECT k FROM b41g)) FROM b41f
SELECT g, array_agg(a) FILTER (WHERE EXISTS (SELECT 1 FROM b41g WHERE k = b41f.a)) FROM b41f GROUP BY g ORDER BY g
SELECT count(*) FILTER (WHERE a IN (SELECT k FROM b41g)), array_agg(a) FILTER (WHERE a IN (SELECT k FROM b41g)) FROM b41f
SELECT json_object_agg(g, a) FILTER (WHERE a IN (SELECT k FROM b41g)) FROM b41f
SELECT array_agg(DISTINCT g) FILTER (WHERE a IN (SELECT k FROM b41g)) FROM b41f
SELECT a, array_agg(a) FILTER (WHERE a IN (SELECT k FROM b41g)) OVER (ORDER BY a) FROM b41f ORDER BY a
# One-sided frames (re-swept for the window-functions entry).
SELECT a, sum(a) OVER (ORDER BY a RANGE BETWEEN 2 PRECEDING AND 1 PRECEDING) FROM b41f ORDER BY a
SELECT a, sum(a) OVER (ORDER BY a DESC RANGE BETWEEN 1 FOLLOWING AND 3 FOLLOWING) FROM b41f ORDER BY a
SELECT a, count(*) OVER (ORDER BY a ROWS BETWEEN 2 FOLLOWING AND 3 FOLLOWING) FROM b41f ORDER BY a
SELECT a, sum(a) OVER (ORDER BY a GROUPS BETWEEN 1 PRECEDING AND 1 PRECEDING EXCLUDE CURRENT ROW) FROM b41f ORDER BY a
# Clean up the reference objects.
DROP SCHEMA b41a CASCADE
DROP SCHEMA b41b CASCADE
DROP TABLE b41f
DROP TABLE b41g
DROP ROLE b41r
