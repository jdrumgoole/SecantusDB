CREATE MATERIALIZED VIEW mv_s AS SELECT g, sum(n) AS total FROM mv_t GROUP BY g
SELECT * FROM mv_s ORDER BY g
INSERT INTO mv_t VALUES (4, 'b', 1)
SELECT * FROM mv_s ORDER BY g
REFRESH MATERIALIZED VIEW mv_s
SELECT * FROM mv_s ORDER BY g
INSERT INTO mv_s VALUES ('z', 1)
UPDATE mv_s SET total = 0
DELETE FROM mv_s
REFRESH MATERIALIZED VIEW mv_t
REFRESH MATERIALIZED VIEW nosuch
CREATE MATERIALIZED VIEW mv_e (a, b) AS SELECT id, n FROM mv_t WHERE n > 5 WITH NO DATA
REFRESH MATERIALIZED VIEW mv_e
SELECT * FROM mv_e ORDER BY a
SELECT matviewname, ispopulated FROM pg_matviews WHERE matviewname LIKE 'mv\_%' ORDER BY 1
SELECT relkind FROM pg_class WHERE relname = 'mv_s'
DROP MATERIALIZED VIEW mv_s
DROP MATERIALIZED VIEW mv_s
DROP MATERIALIZED VIEW IF EXISTS mv_s
SELECT count(*) FROM mv_e
CREATE TABLE ins_a (g text, total int)
INSERT INTO ins_a SELECT g, sum(n) FROM mv_t GROUP BY g
SELECT * FROM ins_a ORDER BY g
INSERT INTO ins_a SELECT t1.g, t2.n FROM mv_t t1 JOIN mv_t t2 ON t1.id = t2.id WHERE t1.id = 1
SELECT count(*) FROM ins_a
CREATE TABLE ins_b AS SELECT g, count(*) c FROM mv_t GROUP BY g
SELECT * FROM ins_b ORDER BY g
COPY (SELECT g, count(*) FROM mv_t GROUP BY g ORDER BY g) TO STDOUT
SELECT pg_size_pretty(0::bigint), pg_size_pretty(1023::bigint), pg_size_pretty(10240::bigint), pg_size_pretty(10485760::bigint), pg_size_pretty(1.5e12::numeric), pg_size_pretty(-2048::bigint)
SELECT pg_size_bytes('10 kB'), pg_size_bytes('1.5 GB'), pg_size_bytes('100')
SELECT pg_typeof(pg_total_relation_size('mv_t')), pg_relation_size('mv_t') >= 0, pg_table_size('mv_t') >= 0, pg_indexes_size('mv_t') >= 0, pg_database_size('postgres') > 0, pg_column_size('abc')
SELECT pg_total_relation_size('nosuch')
SELECT pg_size_pretty(pg_total_relation_size('mv_t')) IS NOT NULL
SELECT relname, pg_relation_size(oid) >= 0 FROM pg_class WHERE relname = 'mv_t'
SELECT count(*) FROM mv_t TABLESAMPLE SYSTEM (100)
SELECT count(*) FROM mv_t TABLESAMPLE BERNOULLI (0)
SELECT count(*) FROM mv_t TABLESAMPLE BERNOULLI (100) REPEATABLE (1)
SELECT count(*) FROM mv_t TABLESAMPLE nosuch (10)
SELECT count(*) FROM mv_t TABLESAMPLE SYSTEM (200)
SELECT count(*) <= 4 FROM mv_t TABLESAMPLE SYSTEM (50)
SELECT count(*) FROM mv_t t TABLESAMPLE SYSTEM (100) WHERE t.n > 5
