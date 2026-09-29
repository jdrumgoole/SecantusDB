SELECT j.id, count(k.id) FROM j14 j LEFT JOIN k14 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.id, sum(k.v) FROM j14 j LEFT JOIN k14 k ON k.jid = j.id GROUP BY j.id ORDER BY j.id
SELECT j.g, array_agg(k.v ORDER BY k.id) FROM j14 j JOIN k14 k ON k.jid = j.id GROUP BY j.g ORDER BY j.g
SELECT j.id FROM j14 j WHERE EXISTS (SELECT 1 FROM k14 k WHERE k.jid = j.id) ORDER BY j.id
SELECT j.id FROM j14 j WHERE NOT EXISTS (SELECT 1 FROM k14 k WHERE k.jid = j.id) ORDER BY j.id
SELECT j.id FROM j14 j WHERE j.id NOT IN (SELECT jid FROM k14) ORDER BY j.id
SELECT j.id FROM j14 j WHERE j.id NOT IN (SELECT jid FROM k14 UNION SELECT NULL) ORDER BY j.id
SELECT j.id, (SELECT count(*) FROM k14 k WHERE k.jid = j.id) FROM j14 j ORDER BY j.id
SELECT j.id, k.v FROM j14 j LEFT JOIN k14 k ON k.jid = j.id AND k.v > 150 ORDER BY j.id, k.v
SELECT count(*) FROM j14 j CROSS JOIN k14 k
SELECT j.id FROM j14 j JOIN k14 k USING (id) ORDER BY j.id
SELECT * FROM j14 NATURAL JOIN k14
SELECT j.id, k.v FROM j14 j FULL JOIN k14 k ON k.jid = j.id ORDER BY j.id NULLS LAST, k.v
SELECT j.id FROM j14 j WHERE j.n IS NOT DISTINCT FROM 10 ORDER BY j.id
SELECT g, count(*) FROM j14 GROUP BY g HAVING count(*) = 1 ORDER BY g
SELECT g, n FROM j14 WHERE n > (SELECT avg(n) FROM j14) ORDER BY g
SELECT j.id FROM j14 j LEFT JOIN k14 k ON k.jid = j.id WHERE k.id IS NULL ORDER BY j.id
SELECT count(DISTINCT j.g) FROM j14 j JOIN k14 k ON k.jid = j.id
SELECT j.id, k.v FROM j14 j LEFT JOIN k14 k ON k.jid = j.id WHERE k.v > 150 ORDER BY j.id, k.v
SELECT j.id, k.v FROM j14 j LEFT JOIN k14 k ON k.jid = j.id WHERE k.v > 150 OR k.v IS NULL ORDER BY j.id
SELECT * FROM j14 j JOIN k14 k ON k.jid = j.id ORDER BY k.id
SELECT j.*, k.v FROM j14 j JOIN k14 k ON k.jid = j.id ORDER BY k.id
SELECT * FROM j14 j JOIN k14 k USING (id)
SELECT * FROM j14 j FULL JOIN k14 k USING (id) ORDER BY id
SELECT id, g FROM j14 JOIN k14 USING (id)
SELECT id FROM j14 j JOIN k14 k ON k.jid = j.id
SELECT j.id, count(*) OVER () FROM j14 j JOIN k14 k ON k.jid = j.id ORDER BY k.id
SELECT j.g, row_number() OVER (PARTITION BY j.g ORDER BY k.v DESC) FROM j14 j JOIN k14 k ON k.jid = j.id ORDER BY j.g, 2
SELECT a.id, b.id FROM j14 a, j14 b WHERE a.id < b.id ORDER BY 1, 2
SELECT j.id, x.n FROM j14 j JOIN (SELECT jid, count(*) AS n FROM k14 GROUP BY jid) x ON x.jid = j.id ORDER BY 1
SELECT j.id, s FROM j14 j CROSS JOIN generate_series(1, 2) s ORDER BY 1, 2
SELECT j.id, k.v, m.v FROM j14 j JOIN k14 k ON k.jid = j.id JOIN k14 m ON m.id = k.id ORDER BY k.id
SELECT j.nosuch FROM j14 j JOIN k14 k ON k.jid = j.id
SELECT x.id FROM j14 j JOIN k14 k ON k.jid = j.id
SELECT * FROM j14 j JOIN j14 j ON true
SELECT DISTINCT j.g FROM j14 j JOIN k14 k ON k.jid = j.id ORDER BY 1
SELECT j.g, sum(k.v) FROM j14 j JOIN k14 k ON k.jid = j.id GROUP BY j.g HAVING sum(k.v) > 250 ORDER BY 1
SELECT j.id, k.v FROM j14 j RIGHT JOIN k14 k ON k.jid = j.id AND k.v < 250 ORDER BY k.v
