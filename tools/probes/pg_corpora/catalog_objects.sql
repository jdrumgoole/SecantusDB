# reference-version: 15
# Record-only statements: extended statistics, publications, tablespaces and
# security labels, with PostgreSQL's validation.
SECURITY LABEL ON TABLE co_t IS 'x'
SECURITY LABEL FOR selinux ON TABLE co_t IS 'x'
CREATE STATISTICS co_st (dependencies) ON a, b FROM co_t
CREATE STATISTICS co_st2 ON a, b FROM co_t
CREATE STATISTICS co_st3 ON a FROM co_t
CREATE STATISTICS co_st ON a, b FROM co_t
CREATE STATISTICS IF NOT EXISTS co_st ON a, b FROM co_t
CREATE STATISTICS co_bad (bogus) ON a, b FROM co_t
CREATE STATISTICS co_st4 ON a, nope FROM co_t
CREATE STATISTICS co_st5 ON (a + b), c FROM co_t
CREATE STATISTICS co_st6 ON a, b FROM co_nope
SELECT stxname, stxkeys::text, stxkind::text FROM pg_statistic_ext WHERE stxname LIKE 'co\_%' ORDER BY 1
SELECT stxname FROM pg_statistic_ext WHERE stxrelid = 'co_t'::regclass ORDER BY 1
ALTER STATISTICS co_st RENAME TO co_stx
ALTER STATISTICS co_stx SET STATISTICS 500
ALTER STATISTICS nope SET STATISTICS 500
ALTER STATISTICS IF EXISTS nope SET STATISTICS 500
DROP STATISTICS co_stx
DROP STATISTICS nope
DROP STATISTICS IF EXISTS nope
CREATE PUBLICATION co_pb FOR TABLE co_t
CREATE PUBLICATION co_pb FOR TABLE co_t
CREATE PUBLICATION co_pb2 FOR TABLE co_t (a, b) WHERE (a > 1), co_u WITH (publish = 'insert, update')
CREATE PUBLICATION co_pb3 FOR TABLE co_nope
CREATE PUBLICATION co_pb3 WITH (publish = 'bogus')
SELECT pubname, puballtables, pubinsert, pubupdate, pubdelete, pubtruncate FROM pg_publication WHERE pubname LIKE 'co\_%' ORDER BY 1
SELECT pubname, tablename, attnames::text FROM pg_publication_tables WHERE pubname LIKE 'co\_%' ORDER BY 1, 2
ALTER PUBLICATION co_pb ADD TABLE co_t
ALTER PUBLICATION co_pb ADD TABLE co_u
ALTER PUBLICATION co_pb DROP TABLE co_t
ALTER PUBLICATION co_pb DROP TABLE co_t
ALTER PUBLICATION co_pb SET (publish = 'insert')
ALTER PUBLICATION co_nope ADD TABLE co_t
SELECT pubname, tablename FROM pg_publication_tables WHERE pubname LIKE 'co\_%' ORDER BY 1, 2
SELECT pubname, pubupdate FROM pg_publication WHERE pubname = 'co_pb'
DROP TABLE co_u
SELECT pubname, tablename FROM pg_publication_tables WHERE pubname LIKE 'co\_%' ORDER BY 1, 2
SELECT count(*) FROM pg_statistic_ext WHERE stxname LIKE 'co\_%'
DROP PUBLICATION co_pb, co_pb2
DROP PUBLICATION co_pb
DROP PUBLICATION IF EXISTS co_pb
CREATE TABLESPACE co_ts LOCATION 'relative/dir'
CREATE TABLESPACE co_ts LOCATION '/definitely/not/here'
CREATE TABLESPACE pg_co LOCATION '/tmp'
DROP TABLESPACE co_nope
DROP TABLESPACE IF EXISTS co_nope
SELECT spcname FROM pg_tablespace ORDER BY 1
CREATE TABLE co_v (a int) TABLESPACE pg_default
CREATE TABLE co_w (a int) TABLESPACE co_nope
DROP TABLE co_v
DROP TABLE co_t
SELECT count(*) FROM pg_statistic_ext WHERE stxname LIKE 'co\_%'
