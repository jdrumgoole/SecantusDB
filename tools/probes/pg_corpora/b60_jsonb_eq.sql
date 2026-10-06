# reference-version: 15
# jsonb compares numbers by VALUE (batch 60): {"x":1} = {"x":1.0}.
SELECT count(*) FROM (SELECT DISTINCT j FROM b60_j) s
SELECT count(*) FROM (SELECT j FROM b60_j GROUP BY j) s
SELECT count(DISTINCT j) FROM b60_j
SELECT count(*) FROM (SELECT j FROM b60_j UNION SELECT j FROM b60_j2) s
SELECT count(*) FROM (SELECT j FROM b60_j INTERSECT SELECT j FROM b60_j2) s
SELECT count(*) FROM (SELECT j FROM b60_j EXCEPT SELECT j FROM b60_j2) s
SELECT count(*) FROM (SELECT DISTINCT ON (j) id FROM b60_j ORDER BY j, id) s
SELECT id FROM b60_j WHERE j = '{"x":1.0}' ORDER BY id
SELECT id FROM b60_j WHERE j = '1.00' ORDER BY id
SELECT id FROM b60_j WHERE j = '{"x":1.0, "y":[2]}'::jsonb ORDER BY id
SELECT id FROM b60_j WHERE j IN ('[1.0]', '100.0') ORDER BY id
SELECT id FROM b60_j WHERE j <> '1' ORDER BY id
SELECT a.id, b.id FROM b60_j a JOIN b60_j2 b ON a.j = b.j ORDER BY 1, 2
SELECT a.id FROM b60_j a WHERE a.j IN (SELECT j FROM b60_j2) ORDER BY 1
SELECT a.id FROM b60_j a WHERE EXISTS (SELECT 1 FROM b60_j2 b WHERE b.j = a.j) ORDER BY 1
SELECT a.id FROM b60_j a WHERE a.j NOT IN (SELECT j FROM b60_j2) ORDER BY 1
SELECT '{"x":1}'::jsonb = '{"x":1.0}'::jsonb
SELECT id, k FROM b60_j WHERE j = (SELECT j FROM b60_j2 WHERE id = 3)
INSERT INTO b60_ju VALUES (2, '{"x":1.0}')
SELECT count(*) FROM b60_ju
SELECT DISTINCT count(*) FROM b60_j GROUP BY k ORDER BY 1
SELECT DISTINCT count(*) AS c FROM b60_j GROUP BY k ORDER BY c DESC
SELECT DISTINCT sum(id) FROM b60_j GROUP BY k ORDER BY 1
UPDATE b60_ju SET j = '{"x":1.00}' WHERE id = 1
INSERT INTO b60_ju VALUES (3, '{"x":2}')
UPDATE b60_ju SET j = '{"x":1.0}' WHERE id = 3
SELECT id FROM b60_j WHERE j IS NOT DISTINCT FROM '[1.0]' ORDER BY id
SELECT id FROM b60_j WHERE j IS DISTINCT FROM '[1.0]' ORDER BY id
SELECT id FROM b60_j WHERE j = ANY (ARRAY['{"x":1.00}', '100']::jsonb[]) ORDER BY id
SELECT id, count(*) OVER (PARTITION BY j) FROM b60_j ORDER BY id
SELECT DISTINCT k, jsonb_agg(j ORDER BY id) - 0 FROM b60_j WHERE k = 1 GROUP BY k
SELECT count(*) FROM (SELECT DISTINCT max(j) FROM b60_j GROUP BY k) s
SELECT j, count(*) FROM b60_j WHERE k IN (1, 3) GROUP BY j ORDER BY 2, 1
