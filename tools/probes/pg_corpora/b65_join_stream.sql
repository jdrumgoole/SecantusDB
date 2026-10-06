# reference-version: 15
# Batch 65: joins of stored tables read in bounded memory -- the rows, and their order, of the materialised path.
SELECT a.id, a.k, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.id ORDER BY a.id, b.name
SELECT a.id, b.name FROM b65_ja a LEFT JOIN b65_jb b ON a.k = b.id ORDER BY a.id, b.name
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.k ORDER BY a.id, b.name
SELECT count(*) FROM b65_ja a JOIN b65_jb b ON a.k = b.k
SELECT a.id, b.name FROM b65_ja a LEFT JOIN b65_jb b ON a.k = b.k WHERE a.id > 590 ORDER BY a.id, b.name
SELECT a.id, b.id FROM b65_ja a JOIN b65_jb b ON a.t = b.name ORDER BY 1, 2
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.id WHERE a.g = 1 ORDER BY a.id LIMIT 5
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.id ORDER BY a.id OFFSET 590
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.id ORDER BY b.name DESC, a.id LIMIT 7
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.id AND b.v > 3 ORDER BY a.id
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.id WHERE a.f + b.v > 300 ORDER BY a.id
SELECT a.id, b.name, c.tag FROM b65_ja a JOIN b65_jb b ON a.k = b.id JOIN b65_jc c ON c.bid = b.id ORDER BY a.id, c.tag
SELECT a.id, b.name, c.tag FROM b65_ja a JOIN b65_jb b ON a.k = b.id LEFT JOIN b65_jc c ON c.bid = b.id WHERE a.id < 20 ORDER BY a.id, c.tag
SELECT a.id, b.name FROM b65_ja a RIGHT JOIN b65_jb b ON a.k = b.id WHERE a.id IS NULL OR a.id < 10 ORDER BY a.id, b.name
SELECT b.name, a.id FROM b65_ja a FULL JOIN b65_jb b ON a.k = b.id WHERE a.id IS NULL OR a.id > 595 ORDER BY 2, 1
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b USING (id) ORDER BY a.id
SELECT id, name FROM b65_ja JOIN b65_jb USING (id, k) ORDER BY id
SELECT * FROM b65_ja a JOIN b65_jb b ON a.k = b.id WHERE a.id < 4 ORDER BY a.id
SELECT a.id, b.name FROM b65_ja a, b65_jb b WHERE a.k = b.id AND a.id < 9 ORDER BY 1, 2
SELECT a.id, b.name FROM b65_ja a CROSS JOIN b65_jb b WHERE a.id < 3 ORDER BY 1, 2
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.f = b.v ORDER BY 1, 2
SELECT a.id, b.name FROM b65_ja a JOIN b65_jb b ON a.k = b.id WHERE b.name LIKE 'n%' AND a.t = 'r1' ORDER BY a.id
# aggregates over a join
SELECT count(*), sum(a.id), max(b.name), min(a.f), avg(a.k) FROM b65_ja a JOIN b65_jb b ON a.k = b.id
SELECT count(*), sum(b.v) FROM b65_ja a LEFT JOIN b65_jb b ON a.k = b.id
SELECT b.name, count(*), sum(a.id), max(a.t) FROM b65_ja a JOIN b65_jb b ON a.k = b.id GROUP BY b.name ORDER BY b.name
SELECT b.name, count(a.id) FROM b65_ja a RIGHT JOIN b65_jb b ON a.k = b.id GROUP BY b.name ORDER BY b.name
SELECT a.g, count(*) FROM b65_ja a JOIN b65_jb b ON a.k = b.id WHERE a.id > 100 GROUP BY a.g HAVING count(*) > 10 ORDER BY 1
SELECT count(DISTINCT b.name) FROM b65_ja a JOIN b65_jb b ON a.k = b.id
SELECT bool_and(a.id > 0), bool_or(b.v > 7) FROM b65_ja a JOIN b65_jb b ON a.k = b.id
SELECT sum(a.f), avg(a.f) FROM b65_ja a JOIN b65_jb b ON a.k = b.id
SELECT count(*) FROM b65_ja a JOIN b65_jb b ON a.k = b.id JOIN b65_jc c ON c.bid = b.id
SELECT a.k, string_agg(b.name, ',' ORDER BY b.name) FROM b65_ja a JOIN b65_jb b ON a.k = b.id WHERE a.id < 30 GROUP BY a.k ORDER BY 1
SELECT max(a.id / (b.id - 3)) FROM b65_ja a JOIN b65_jb b ON a.k = b.id
