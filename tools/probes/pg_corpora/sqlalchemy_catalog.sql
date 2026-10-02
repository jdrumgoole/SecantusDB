# What SQLAlchemy's postgresql dialect reads during reflection, pinned
# against PostgreSQL independently of the dialect suite (batch 35).
# pg_table_is_visible: a relation outside the search path is not visible.
SELECT c.relname, pg_table_is_visible(c.oid) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relname IN ('sql_parts', 'tables', 'sa35_p', 'pg_class', 'sa35_d', 'sa35_d_ix', 'sa35_v') ORDER BY 1, 2
SELECT pg_table_is_visible('information_schema.sql_parts'::regclass), pg_table_is_visible('sa35_p'::regclass), pg_table_is_visible('sa35_s.sa35_d'::regclass), pg_table_is_visible(0)
SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p') AND pg_table_is_visible(c.oid) AND n.nspname != 'pg_catalog' AND c.relname IN ('sql_parts', 'sql_sizing', 'sa35_p', 'sa35_d') ORDER BY 1
SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('v', 'm') AND pg_table_is_visible(c.oid) AND n.nspname != 'pg_catalog' AND c.relname IN ('tables', 'columns', 'sa35_v') ORDER BY 1
# pg_get_constraintdef quotes identifiers the way ruleutils does.
SELECT conname, pg_get_constraintdef(oid, true) FROM pg_constraint WHERE conrelid = 'sa35_c'::regclass ORDER BY conname
SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'sa35_s.sa35_d'::regclass ORDER BY conname
# Constraint comments live in pg_description under pg_constraint.
SELECT c.conname, d.description, d.classoid::regclass::text FROM pg_description d JOIN pg_constraint c ON c.oid = d.objoid WHERE c.conrelid IN ('sa35_s.sa35_d'::regclass, 'sa35_s.sa35_e'::regclass) ORDER BY 1
SELECT conname, obj_description(oid, 'pg_constraint') FROM pg_constraint WHERE conrelid = 'sa35_s.sa35_d'::regclass ORDER BY conname
SELECT obj_description('sa35_s.sa35_d_ix'::regclass, 'pg_class')
# An index name is per schema.
SELECT schemaname, tablename, indexname FROM pg_indexes WHERE indexname LIKE 'sa35_u%' ORDER BY 1, 3
SELECT 'sa35_s.sa35_u_ix'::regclass::text, 'sa35_u_ix'::regclass::text
DROP INDEX sa35_s.sa35_u_ix
SELECT schemaname, indexname FROM pg_indexes WHERE indexname LIKE 'sa35_u%' ORDER BY 1, 2
DROP INDEX sa35_s.sa35_u_a_idx
SELECT schemaname, indexname FROM pg_indexes WHERE indexname LIKE 'sa35_u%' ORDER BY 1, 2
# A declared PRIMARY KEY order is the constraint's column order.
SELECT conkey, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'sa35_pk'::regclass
SELECT i.indkey::text FROM pg_index i WHERE i.indrelid = 'sa35_pk'::regclass
SELECT kcu.column_name, kcu.ordinal_position FROM information_schema.key_column_usage kcu WHERE kcu.table_name = 'sa35_pk' ORDER BY 2
# INCLUDE columns are in indkey / indnatts but not indnkeyatts.
SELECT i.indnatts, i.indnkeyatts, i.indkey::text FROM pg_index i WHERE i.indexrelid = 'sa35_ci_ix'::regclass
SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey) WHERE i.indexrelid = 'sa35_ci_ix'::regclass ORDER BY 1
# A cast over a joined column keeps the column's name.
SELECT d.classoid::regclass, d.objsubid FROM pg_description d JOIN pg_constraint c ON c.oid = d.objoid WHERE c.conname = 'sa35_d_fk'
SELECT t.oid::regtype::text, t.typname::text FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE t.typname = 'int4'
# An uncorrelated scalar subquery keeps its column's type and name.
SELECT (SELECT max(d) FROM sa35_ts)
SELECT pg_typeof((SELECT d FROM sa35_ts WHERE id = 1)), (SELECT d FROM sa35_ts WHERE id = 1) AS anon_1
SELECT (SELECT id FROM sa35_ts ORDER BY id LIMIT %s) AS a ||| [1]
SELECT s.a FROM (SELECT (SELECT id FROM sa35_ts ORDER BY id LIMIT %s) AS a UNION SELECT (SELECT id FROM sa35_ts ORDER BY id LIMIT %s)) AS s ||| [1, 1]
SELECT (SELECT id FROM sa35_ts) AS a
# A sequence created in the open transaction is visible to it.
BEGIN
CREATE SEQUENCE sa35_seq
SELECT relname, pg_table_is_visible(oid) FROM pg_class WHERE relkind = 'S' AND relname = 'sa35_seq'
ROLLBACK
SELECT count(*) FROM pg_class WHERE relname = 'sa35_seq'
# A cursor declared in a transaction sees that transaction's own rows.
BEGIN
INSERT INTO sa35_ts VALUES (3, '2008-01-01 00:00:00')
DECLARE sa35_cur CURSOR FOR SELECT id FROM sa35_ts ORDER BY id
FETCH ALL FROM sa35_cur
ROLLBACK
# In a transaction, an index dropped with its table frees its name.
BEGIN
DROP TABLE sa35_ci
CREATE TABLE sa35_ci (x int, y int, z int)
CREATE INDEX sa35_ci_ix ON sa35_ci (x)
SELECT indexname FROM pg_indexes WHERE indexname = 'sa35_ci_ix'
ROLLBACK
# Leave nothing behind on the reference server (information_schema counts
# in other corpora see a stray schema's tables).
DROP VIEW sa35_v
DROP TABLE sa35_c, sa35_p, sa35_u, sa35_pk, sa35_ci, sa35_ts
DROP TABLE sa35_s.sa35_d, sa35_s.sa35_e, sa35_s.sa35_u
DROP SCHEMA sa35_s
SELECT count(*) FROM pg_class WHERE relname LIKE 'sa35%'
