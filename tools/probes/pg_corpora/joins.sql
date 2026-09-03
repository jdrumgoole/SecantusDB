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
