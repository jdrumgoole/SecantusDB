# reference-version: 15
# Batch 67: an aggregate belongs to the lowest query level whose variables it reads (PostgreSQL's agglevelsup).
SELECT (SELECT max(s.x) FROM b67_t) FROM (SELECT a * 2 AS x FROM b67_t) s
SELECT (SELECT max(s.x) FROM b67_t LIMIT 1) FROM (SELECT a * 2 AS x FROM b67_t) s
SELECT (SELECT max(s.x)) FROM b67_s s
SELECT (SELECT max(s.x) + 1) FROM b67_s s
SELECT (SELECT sum(s.x) FROM b67_t WHERE a = 1) FROM b67_s s
SELECT (SELECT sum(s.x) FROM b67_t WHERE a = 9) FROM b67_s s
SELECT (SELECT count(s.x) + count(*) FROM b67_t) FROM b67_s s
SELECT (SELECT count(*) FROM b67_t WHERE a < max(s.x) / 10) FROM b67_s s
SELECT (SELECT max(x)) FROM b67_s s
SELECT (SELECT max(x) FROM b67_t) FROM b67_s s
SELECT (SELECT max(a) FROM b67_t) FROM b67_s s ORDER BY 1
SELECT (SELECT max(s.x + t.a) FROM b67_t t) FROM b67_s s ORDER BY 1
SELECT EXISTS (SELECT max(s.x) FROM b67_t) FROM b67_s s
SELECT EXISTS (SELECT 1 FROM b67_t WHERE a = max(s.x) / 10) FROM b67_s s
SELECT 2 IN (SELECT max(s.y) FROM b67_t) FROM b67_s s
SELECT max(s.y) IN (SELECT a FROM b67_t) FROM b67_s s
SELECT s.y, (SELECT max(s.x)) FROM b67_s s GROUP BY s.y ORDER BY 1
SELECT s.y FROM b67_s s GROUP BY s.y HAVING (SELECT sum(s.x)) > 25 ORDER BY 1
SELECT s.y FROM b67_s s GROUP BY s.y HAVING EXISTS (SELECT 1 FROM b67_t WHERE a * 10 < max(s.x)) ORDER BY 1
SELECT s.y, (SELECT count(*) FROM b67_t WHERE a <= count(s.x)) FROM b67_s s GROUP BY s.y ORDER BY 1
SELECT s.x FROM b67_s s WHERE (SELECT max(s.x)) > 0
SELECT s.x FROM b67_s s WHERE EXISTS (SELECT 1 FROM b67_t WHERE a = max(s.x))
SELECT s.x FROM b67_s s JOIN b67_t t ON (SELECT max(s.x)) > 0
SELECT count(*) FROM b67_s s GROUP BY (SELECT max(s.x))
SELECT (SELECT max(s.x)), s.y FROM b67_s s
SELECT s.x, (SELECT max(s.x)) FROM b67_s s
SELECT (SELECT (SELECT max(s.x) FROM b67_t t2) FROM b67_t t1) FROM b67_s s
SELECT (SELECT (SELECT max(t1.a) FROM b67_t t2) FROM b67_t t1) FROM b67_s s ORDER BY 1
SELECT (SELECT (SELECT max(t1.a) FROM b67_t t2 LIMIT 1) FROM b67_t t1 LIMIT 1) FROM b67_s s ORDER BY 1
SELECT (SELECT max(s.x) FROM b67_t t1 WHERE t1.a = 1) + 1 FROM b67_s s
SELECT (SELECT max(s.x)) FROM b67_s s ORDER BY 1
SELECT (SELECT max(s.x)) FROM b67_s s WHERE s.y = 1
SELECT (SELECT max(s.x) FILTER (WHERE s.y = 1)) FROM b67_s s
SELECT (SELECT string_agg(s.x::text, ',' ORDER BY s.x)) FROM b67_s s
SELECT (SELECT max(s.x) OVER ()) FROM b67_s s ORDER BY 1
SELECT (SELECT max(s.x)) AS m FROM b67_s s LIMIT 1
SELECT (SELECT max(s.x) FROM b67_t) FROM b67_s s WHERE false
SELECT DISTINCT (SELECT max(s.x)) FROM b67_s s
SELECT (SELECT s.x) FROM b67_s s
SELECT (SELECT s.x FROM b67_t LIMIT 1) FROM b67_s s
SELECT (SELECT g.m) FROM (SELECT max(x) AS m FROM b67_s) g
SELECT (SELECT g.m) FROM (SELECT max(x) AS m FROM b67_s GROUP BY y) g ORDER BY 1
SELECT (SELECT max(t1.a) FROM b67_t t2 LIMIT 1) FROM b67_t t1
SELECT max(1) OVER ()
SELECT max(x) OVER () FROM (SELECT 1 AS x) q
SELECT (SELECT s.x + 0 OVER ()) FROM b67_s s
SELECT (SELECT sum(s.x) OVER ()) FROM b67_s s ORDER BY 1
SELECT (SELECT row_number() OVER () + s.x) FROM b67_s s ORDER BY 1
SELECT ARRAY(SELECT max(s.x) FROM b67_t) FROM b67_s s
SELECT (SELECT s.y + max(s.x)) FROM b67_s s
SELECT s.y, (SELECT s.x + max(s.x)) FROM b67_s s GROUP BY s.y
SELECT (SELECT max(x) FROM b67_t WHERE a = 1) FROM b67_s
SELECT (SELECT max(y) + count(*) FROM b67_t) FROM b67_s
SELECT (SELECT max(a) + max(y) FROM b67_t) FROM b67_s ORDER BY 1
SELECT (SELECT (SELECT max(s.x) + max(t1.a) FROM b67_t t2 LIMIT 1) FROM b67_t t1 LIMIT 1) FROM b67_s s ORDER BY 1
SELECT (SELECT (SELECT max(s.x) + t1.a FROM b67_t t2 LIMIT 1) FROM b67_t t1 ORDER BY 1 LIMIT 1) FROM b67_s s
WITH c AS (SELECT x FROM b67_s) SELECT (SELECT max(c.x) FROM b67_t) FROM c
WITH c AS (SELECT x FROM b67_s) SELECT (SELECT max(c.x) FROM b67_t LIMIT 1) FROM c
SELECT * FROM (SELECT (SELECT max(s.x)) AS m FROM b67_s s) q
SELECT y, (SELECT max(q.x) FROM b67_t) FROM (SELECT x, y FROM b67_s) q GROUP BY y ORDER BY 1
SELECT s.y, (SELECT max(s.x) FROM b67_t LIMIT 1) FROM b67_s s GROUP BY s.y HAVING (SELECT count(s.x)) > 1
SELECT (SELECT max(s.x)) FROM b67_s s UNION ALL SELECT (SELECT min(s.x)) FROM b67_s s ORDER BY 1
SELECT (SELECT count(*) FROM b67_t WHERE a > 1) FROM b67_s s ORDER BY 1
