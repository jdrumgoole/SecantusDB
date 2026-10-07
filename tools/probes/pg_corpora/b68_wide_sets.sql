# reference-version: 15
# Batch 68: grouping sets over WIDE aggregate inputs (max(pad)) grouped from one read, each set's groups hashed with their partial results combined; joins whose right side is past the bound with the left side held in memory (run also with SECANTUS_PG_GROUP_MEMORY_BYTES=20000 / 1 and SECANTUS_PG_JOIN_INNER_BYTES=20000 / 1).
SELECT g, k % 3, count(*), max(pad), min(pad) FROM b68_w GROUP BY GROUPING SETS ((g), (k % 3), ()) ORDER BY GROUPING(g), GROUPING(k % 3), 1, 2, 3
SELECT g, k, count(pad), sum(id), sum(n), max(pad) FROM b68_w GROUP BY ROLLUP (g, k) ORDER BY GROUPING(g, k), 1, 2
SELECT g, k, bool_and(b), bool_or(b), max(pad) FROM b68_w GROUP BY CUBE (g, k) ORDER BY GROUPING(g, k), 1, 2
SELECT j, count(*), max(pad) FROM b68_w GROUP BY ROLLUP (j) ORDER BY GROUPING(j), 1, 2
SELECT n, count(*), max(pad) FROM b68_w WHERE id > 1490 GROUP BY ROLLUP (n) ORDER BY GROUPING(n), 1, 2
SELECT g, count(*), max(pad) FROM b68_w WHERE id > 99999 GROUP BY GROUPING SETS ((g), ()) ORDER BY 1
SELECT g, k, count(*), max(pad), GROUPING(g, k) FROM b68_w GROUP BY GROUPING SETS ((g), (g), (k)) ORDER BY 1, 2, 3
SELECT g, k, count(*), max(pad) FROM b68_w GROUP BY ROLLUP (g, k) HAVING count(*) > 40 ORDER BY GROUPING(g, k), 1, 2
SELECT g, avg(id), max(pad) FROM b68_w GROUP BY ROLLUP (g) ORDER BY GROUPING(g), 1
SELECT g, max(upper(pad)), count(*) FROM b68_w GROUP BY ROLLUP (g) ORDER BY GROUPING(g), 1
SELECT s.name, w.g, count(*), max(w.pad) FROM b68_w w JOIN b68_s s ON w.k = s.id GROUP BY ROLLUP (s.name, w.g) ORDER BY GROUPING(s.name, w.g), 1, 2
# a right side past the bound against a left side that fits
SELECT s.id, w.id FROM b68_s s JOIN b68_w w ON w.k = s.id WHERE w.id < 60 ORDER BY 1, 2
SELECT count(*), count(w.id), count(s.id), sum(w.id) FROM b68_s s JOIN b68_w w ON w.k = s.id
SELECT count(*), count(w.id), count(s.id) FROM b68_s s LEFT JOIN b68_w w ON w.k = s.id
SELECT count(*), count(w.id), count(s.id) FROM b68_s s RIGHT JOIN b68_w w ON w.k = s.id
SELECT count(*), count(w.id), count(s.id) FROM b68_s s FULL JOIN b68_w w ON w.k = s.id
SELECT s.name, w.id FROM b68_s s LEFT JOIN b68_w w ON w.k = s.id AND w.id < 20 ORDER BY 1, 2
SELECT s.name, w.id FROM b68_s s FULL JOIN b68_w w ON w.k = s.id AND w.id < 20 WHERE s.id IS NULL OR w.id IS NULL OR s.id > 5 ORDER BY 1, 2 LIMIT 30
SELECT s.name, w.id FROM b68_s s JOIN b68_w w ON w.k = s.id ORDER BY 2, 1 LIMIT 10 OFFSET 700
SELECT s.name, w.id FROM b68_s s LEFT JOIN b68_w w ON w.n = s.id ORDER BY 1, 2
SELECT s.name, count(*) FROM b68_s s JOIN b68_w w ON w.k = s.id GROUP BY s.name ORDER BY 1
# GROUPING() over a join (was 42803 everywhere: the join rewrite did not reach its arguments)
SELECT s.name, w.g, GROUPING(s.name, w.g), count(*) FROM b68_w w JOIN b68_s s ON w.k = s.id GROUP BY ROLLUP (s.name, w.g) ORDER BY 3, 1, 2
SELECT s.name, count(*) FROM b68_w w JOIN b68_s s ON w.k = s.id GROUP BY ROLLUP (s.name) ORDER BY GROUPING(name), 1
SELECT w.g, count(*) FROM b68_w w, b68_s s WHERE w.k = s.id GROUP BY CUBE (w.g) HAVING GROUPING(w.g) = 1 OR w.g > 1 ORDER BY 1
# an ORDER BY expression names INPUT columns; only a bare key may name an output
SELECT s.name AS nm, count(*) FROM b68_w w JOIN b68_s s ON w.k = s.id GROUP BY s.name ORDER BY nm || 'x'
SELECT s.name, count(*) AS c FROM b68_w w JOIN b68_s s ON w.k = s.id GROUP BY s.name ORDER BY c + 1
SELECT s.name AS g, w.id FROM b68_w w JOIN b68_s s ON w.k = s.id WHERE w.id < 30 ORDER BY -g, 2, 1
SELECT s.name AS g, w.id FROM b68_w w JOIN b68_s s ON w.k = s.id WHERE w.id < 30 ORDER BY g, 2
SELECT s.name, w.id FROM b68_w w JOIN b68_s s ON w.k = s.id WHERE w.id < 30 ORDER BY name || 'x', 2
# GROUPING() in HAVING (was 0A000 `this HAVING term`)
SELECT g, k, count(*) FROM b68_w GROUP BY ROLLUP (g, k) HAVING GROUPING(g, k) > 0 ORDER BY GROUPING(g, k), 1, 2
SELECT g, k, count(*) FROM b68_w GROUP BY ROLLUP (g, k) HAVING GROUPING(k) = 1 AND count(*) > 300 ORDER BY 1, 2
SELECT g, count(*) FROM b68_w GROUP BY g HAVING 0 = GROUPING(g) ORDER BY 1
SELECT g, count(*) FROM b68_w GROUP BY g HAVING GROUPING(k) = 0
SELECT GROUPING(k), count(*) FROM b68_w GROUP BY g
SELECT g, count(*) FROM b68_w GROUP BY g ORDER BY GROUPING(k)
SELECT g, k, GROUPING(g, k), count(*) FROM b68_w GROUP BY CUBE (g, k) HAVING NOT GROUPING(g, k) = 3 AND count(*) > 100 ORDER BY 3, 1, 2
