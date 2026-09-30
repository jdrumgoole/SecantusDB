INSERT INTO m VALUES (1, 1, 'a'), (10, 4, 'b'), (10, 5, 'c'), (15, 0, 'd'), (19, 100, 'e')
SELECT a, b FROM m_lo ORDER BY 1, 2
SELECT a, b FROM m_mid ORDER BY 1, 2
INSERT INTO m VALUES (20, 0, 'x')
INSERT INTO m VALUES (1, 1, 'dup')
INSERT INTO m VALUES (1, 1, 'up') ON CONFLICT (a, b) DO UPDATE SET v = excluded.v
SELECT v FROM m WHERE a = 1
UPDATE m SET a = 12 WHERE a = 1
SELECT a, b FROM m_mid ORDER BY 1, 2
UPDATE m_mid SET a = 1 WHERE a = 12
UPDATE m SET a = 50 WHERE a = 12
SELECT pg_get_expr(relpartbound, oid) FROM pg_class WHERE relname IN ('m_lo', 'm_mid') ORDER BY relname
CREATE TABLE m_bad PARTITION OF m FOR VALUES FROM (15, 0) TO (30, 0)
CREATE TABLE m_empty PARTITION OF m FOR VALUES FROM (30, 0) TO (30, 0)
CREATE TABLE m_arity PARTITION OF m FOR VALUES FROM (30) TO (40)
CREATE TABLE m_list PARTITION OF m FOR VALUES IN (1)
CREATE TABLE np_p PARTITION OF np FOR VALUES IN (1)
CREATE TABLE bad_key (x int) PARTITION BY RANGE (y)
CREATE TABLE bad_list (x int, y int) PARTITION BY LIST (x, y)
INSERT INTO s VALUES (1, 'eu', 2010), (2, 'eu', 2021), (3, NULL, 2000), (4, 'xx', 1)
INSERT INTO s VALUES (5, 'eu', 1990)
INSERT INTO s VALUES (6, 'us', 2000)
SELECT id FROM s_eu ORDER BY id
SELECT id FROM s_eu_new ORDER BY id
SELECT id FROM s_null ORDER BY id
SELECT tableoid::regclass::text, id FROM s ORDER BY id
UPDATE s SET yr = 2030 WHERE id = 1
SELECT id FROM s_eu_new ORDER BY id
INSERT INTO s_eu VALUES (7, 'eu', 2050)
INSERT INTO s_eu VALUES (8, 'us', 2050)
SELECT relname, relkind FROM pg_class WHERE relname LIKE 's\_%' OR relname = 's' ORDER BY 1
SELECT partstrat, partnatts, partattrs FROM pg_partitioned_table p JOIN pg_class c ON c.oid = p.partrelid WHERE c.relname IN ('m', 's', 's_eu') ORDER BY c.relname
ALTER TABLE s ADD COLUMN note text
SELECT column_name FROM information_schema.columns WHERE table_name = 's_eu_old' ORDER BY ordinal_position
INSERT INTO s VALUES (9, 'eu', 2001, 'hi')
SELECT note FROM s_eu_old WHERE id = 9
DELETE FROM s_eu WHERE yr < 2020
SELECT count(*) FROM s
TRUNCATE s_null
SELECT id FROM s ORDER BY id
ALTER TABLE s DETACH PARTITION s_eu
SELECT id FROM s_eu ORDER BY id
SELECT id FROM s ORDER BY id
INSERT INTO s_eu VALUES (10, 'zz', 2050)
CREATE TABLE lone (id int, region text, yr int, note text)
INSERT INTO lone VALUES (11, 'us', 1)
ALTER TABLE s ATTACH PARTITION lone FOR VALUES IN ('eu')
ALTER TABLE s ATTACH PARTITION lone FOR VALUES IN ('us')
SELECT tableoid::regclass::text, id FROM s ORDER BY id
DROP TABLE s_eu
