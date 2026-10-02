# reference-version: 15
# Batch 45: sum/avg/... over a type no overload takes is 42883 (sum(text)
# answered 0); an aggregate in WHERE is 42803; INSERT / UPDATE name the
# relation of a missing target column. (Positions and hints: slice tests.)
SELECT a, count(*) FROM b45e
SELECT b, count(*) FROM b45e GROUP BY a
SELECT coalesce(a, 0), count(*) FROM b45e
SELECT b45e.a FROM b45e x
SELECT q.a FROM b45e
SELECT zz FROM b45e
SELECT a FROM b45e WHERE b = 1
SELECT a FROM b45e WHERE a = 'x'
SELECT a FROM b45e ORDER BY zz
SELECT * FROM b45e x JOIN b45e y ON x.a = y.zz
SELECT a + b FROM b45e
SELECT a FROM nosuch
SELECT nosuchfn(a) FROM b45e
SELECT a FROM b45e GROUP BY 5
SELECT a FROM b45e ORDER BY 5
SELECT a, a FROM b45e WHERE c = 'q'
SELECT b FROM b45e WHERE b = 'x' AND b > 3
INSERT INTO b45e (zz) VALUES (1)
UPDATE b45e SET zz = 1
SELECT sum(b) FROM b45e
SELECT a FROM b45e WHERE sum(a) > 1
SELECT * FROM b45e x, (SELECT b45e.a FROM b45e y) s
SELECT b45e.zz FROM b45e x
SELECT a FROM b45e ORDER BY a, 7
SELECT a, b FROM b45e GROUP BY 1, 9
SELECT avg(b) FROM b45e
SELECT bool_and(a) FROM b45e
SELECT sum(a::text) FROM b45e
SELECT sum(x) FROM (SELECT b AS x FROM b45e) s
SELECT a FROM b45e WHERE a > 0 AND count(*) > 0
UPDATE b45e SET a = 1 WHERE max(a) > 0
DELETE FROM b45e WHERE sum(a) > 0
SELECT a FROM b45e WHERE a IN (SELECT max(c) FROM b45e)
SELECT a FROM b45e GROUP BY a HAVING sum(b) > 0
INSERT INTO b45e (a, zz) SELECT 1, 2
SELECT stddev(b) FROM b45e
SELECT sum(a) FROM b45e WHERE b = 'x'
