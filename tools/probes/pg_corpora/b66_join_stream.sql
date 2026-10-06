# reference-version: 15
# Batch 66: streamed joins -- a subquery / function / LATERAL side, a right side past the bound (grace hash join), inside a transaction block.
SELECT a.id, s.name FROM b66_ja a JOIN (SELECT id, name FROM b66_jb WHERE id < 4) s ON a.k = s.id ORDER BY a.id, s.name
SELECT a.id, s.n FROM b66_ja a LEFT JOIN (SELECT id, count(*) AS n FROM b66_jc JOIN b66_jb ON b66_jc.bid = b66_jb.id GROUP BY id) s ON a.k = s.id WHERE a.id < 30 ORDER BY 1, 2
SELECT s.k, b.name FROM (SELECT k FROM b66_ja WHERE id < 20) s JOIN b66_jb b ON s.k = b.id ORDER BY 1, 2
SELECT a.id, g FROM b66_ja a JOIN generate_series(0, 3) g ON a.k = g WHERE a.id < 40 ORDER BY 1, 2
SELECT a.id, l.x FROM b66_ja a, LATERAL (SELECT a.k * 2 AS x) l WHERE a.id < 25 ORDER BY 1
SELECT a.id, l.x FROM b66_ja a LEFT JOIN LATERAL (SELECT b.id AS x FROM b66_jb b WHERE b.id = a.k AND b.id > 2) l ON true WHERE a.id < 25 ORDER BY 1, 2
SELECT a.id, u FROM b66_ja a, LATERAL unnest(ARRAY[a.k, a.g]) u WHERE a.id < 10 ORDER BY 1, 2
SELECT count(*), sum(a.id) FROM b66_ja a JOIN (SELECT id FROM b66_jb) s ON a.k = s.id
# a right side past the bound: partitioned (the corpus is also run with SECANTUS_PG_JOIN_INNER_BYTES=1)
SELECT a.id, d.id FROM b66_ja a JOIN b66_jd d ON a.k = d.k WHERE a.id < 8 ORDER BY 1, 2
SELECT count(*), sum(d.id), max(a.t) FROM b66_ja a JOIN b66_jd d ON a.k = d.k
SELECT count(*), count(d.id), count(a.id) FROM b66_ja a LEFT JOIN b66_jd d ON a.k = d.k
SELECT count(*), count(d.id), count(a.id) FROM b66_ja a RIGHT JOIN b66_jd d ON a.k = d.k
SELECT count(*), count(d.id), count(a.id) FROM b66_ja a FULL JOIN b66_jd d ON a.k = d.k
SELECT a.id, d.id FROM b66_ja a FULL JOIN b66_jd d ON a.id = d.id WHERE a.id IS NULL OR d.id IS NULL ORDER BY 1, 2
SELECT d.k, count(*) FROM b66_jd d JOIN b66_ja a ON a.k = d.k GROUP BY d.k ORDER BY 1
SELECT a.id, b.name, d.id FROM b66_ja a JOIN b66_jb b ON a.k = b.id JOIN b66_jd d ON d.k = b.id WHERE a.id < 6 ORDER BY 1, 2, 3
SELECT a.id, d.id FROM b66_ja a JOIN b66_jd d ON a.t = d.pad ORDER BY 1, 2
SELECT a.id, d.id FROM b66_ja a JOIN b66_jd d ON a.k = d.k AND d.id > 2990 ORDER BY 1, 2
SELECT a.id, d.id FROM b66_ja a JOIN b66_jd d ON a.f = d.k WHERE a.id < 10 ORDER BY 1, 2
SELECT a.id, d.id FROM b66_ja a JOIN b66_jd d ON a.k = d.k ORDER BY 1, 2 LIMIT 5 OFFSET 100
# a float8 / numeric key joined to an int one compares by value (it matched nothing on the narrow path)
SELECT count(*) FROM b66_ja a JOIN b66_jd d ON a.f = d.k
SELECT a.id, d.id FROM b66_ja a JOIN b66_jd d ON a.f = d.k ORDER BY 1, 2 LIMIT 12
SELECT a.id, b.id FROM b66_ja a JOIN b66_jb b ON a.f = b.v ORDER BY 1, 2
SELECT b.id, d.id FROM b66_jb b JOIN b66_jd d ON b.v = d.k ORDER BY 1, 2 LIMIT 20
SELECT count(*) FROM b66_ja a LEFT JOIN b66_jd d ON a.f = d.k
SELECT count(*) FROM b66_jd d JOIN b66_ja a ON d.k = a.f
# grouping sets in bounded memory (also run with SECANTUS_PG_GROUP_MEMORY_BYTES=1)
SELECT g, k, count(*), sum(id) FROM b66_ja GROUP BY GROUPING SETS ((g), (k), ()) ORDER BY 1, 2, 3
SELECT g, k, count(*), GROUPING(g, k) FROM b66_ja GROUP BY ROLLUP (g, k) ORDER BY 1, 2, 3
SELECT g, t, max(f), min(t) FROM b66_ja GROUP BY CUBE (g, t) HAVING count(*) > 10 ORDER BY 1, 2, 3
SELECT a.g, b.name, count(*) FROM b66_ja a JOIN b66_jb b ON a.k = b.id GROUP BY GROUPING SETS ((a.g), (b.name), ()) ORDER BY 1, 2, 3
SELECT d.k % 5 AS m, count(*), sum(a.id) FROM b66_ja a JOIN b66_jd d ON a.k = d.k GROUP BY ROLLUP (d.k % 5) ORDER BY 1, 2
SELECT g, count(*) FROM b66_ja WHERE id > 1000 GROUP BY GROUPING SETS ((g), ()) ORDER BY 1, 2
SELECT k, string_agg(t, ',' ORDER BY id) FROM b66_ja WHERE id < 20 GROUP BY GROUPING SETS ((k), ()) ORDER BY 1, 2
SELECT g, count(DISTINCT k) FROM b66_ja GROUP BY ROLLUP (g) ORDER BY 1, 2
# inside a transaction block
BEGIN ISOLATION LEVEL REPEATABLE READ
INSERT INTO b66_jb VALUES (6, 'n6', 6, 9)
SELECT a.id, b.name FROM b66_ja a JOIN b66_jb b ON a.k = b.id WHERE a.id < 15 ORDER BY 1, 2
DELETE FROM b66_ja WHERE id = 3
SELECT count(*), sum(a.id) FROM b66_ja a JOIN b66_jb b ON a.k = b.id
SELECT a.id, d.id FROM b66_ja a JOIN b66_jd d ON a.k = d.k WHERE a.id < 5 ORDER BY 1, 2
SELECT a.id, s.name FROM b66_ja a JOIN (SELECT id, name FROM b66_jb WHERE id > 4) s ON a.k = s.id ORDER BY 1, 2
ROLLBACK
BEGIN
UPDATE b66_jb SET name = 'renamed' WHERE id = 2
SELECT a.id, b.name FROM b66_ja a JOIN b66_jb b ON a.k = b.id WHERE a.id < 12 ORDER BY 1, 2
SELECT b.name, count(*) FROM b66_ja a JOIN b66_jb b ON a.k = b.id GROUP BY b.name ORDER BY 1
SELECT a.id / (b.id - 2) FROM b66_ja a JOIN b66_jb b ON a.k = b.id
SELECT 1
ROLLBACK
SELECT a.id, b.name FROM b66_ja a JOIN b66_jb b ON a.k = b.id WHERE a.id < 12 ORDER BY 1, 2
