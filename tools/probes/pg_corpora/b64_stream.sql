# reference-version: 15
# Batch 64: indexed WHERE streams through its index a batch at a time; the answers are the materialised ones.
SELECT id, k FROM b64_s WHERE k = 3 ORDER BY id
SELECT count(*) FROM (SELECT id FROM b64_s WHERE k >= 5) s
SELECT id, k, t FROM b64_s WHERE k >= 8 AND g = 1 ORDER BY id
SELECT id FROM b64_s WHERE k IN (1, 2) AND id < 30 ORDER BY id
SELECT id FROM b64_s WHERE (k = 1 OR k = 2) AND id < 40 ORDER BY id
SELECT id FROM b64_s WHERE t = 'w2' AND id > 180 ORDER BY id
SELECT id FROM b64_s WHERE t >= 'w3' ORDER BY id LIMIT 5
SELECT id FROM b64_s WHERE k = 4 ORDER BY id DESC LIMIT 3 OFFSET 2
SELECT id FROM b64_s WHERE id = 17
SELECT id FROM b64_s WHERE id IN (3, 5, 1000)
SELECT id FROM b64_s WHERE id > 195 ORDER BY id
SELECT DISTINCT k FROM b64_s WHERE k > 6 ORDER BY k
# aggregates over an indexed filter
SELECT count(*), sum(id), min(t), max(k), bool_and(g < 3) FROM b64_s WHERE k >= 7
SELECT avg(d), sum(d), avg(n), sum(n) FROM b64_s WHERE k = 2
SELECT count(*) FROM b64_s WHERE k = 99
SELECT g, count(*), sum(id) FROM b64_s WHERE k >= 5 GROUP BY g ORDER BY g
SELECT k, max(d) FROM b64_s WHERE t = 'w1' GROUP BY k ORDER BY k
# DISTINCT over tsvector / tsquery
SELECT DISTINCT v FROM b64_s ORDER BY 1::text
SELECT count(*) FROM (SELECT DISTINCT v FROM b64_s) s
SELECT count(*) FROM (SELECT DISTINCT q FROM b64_s) s
SELECT count(*) FROM (SELECT DISTINCT v, g FROM b64_s) s
SELECT count(*) FROM (SELECT DISTINCT v, q FROM b64_s WHERE k >= 5) s
# inside a block, the block's own writes are seen
BEGIN
INSERT INTO b64_s VALUES (1001, 3, 0, 0, 0, 'w9', to_tsvector('simple', 'zz'), to_tsquery('simple', 'zz'))
SELECT id FROM b64_s WHERE k = 3 AND id > 190 ORDER BY id
SELECT count(*) FROM b64_s
SELECT id, t FROM b64_s WHERE t = 'w9'
SELECT count(*) FROM (SELECT DISTINCT v FROM b64_s) s
DELETE FROM b64_s WHERE k = 3 AND id < 100
SELECT count(*) FROM b64_s WHERE k = 3
SELECT count(*), sum(id) FROM b64_s WHERE k >= 3
ROLLBACK
SELECT count(*) FROM b64_s WHERE k = 3
