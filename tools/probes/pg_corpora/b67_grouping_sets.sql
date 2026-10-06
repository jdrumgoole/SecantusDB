# reference-version: 15
# Batch 67: GROUPING SETS / ROLLUP / CUBE grouped from one read of the input (run also with SECANTUS_PG_GROUP_MEMORY_BYTES=1).
SELECT g, k, count(*), sum(id), avg(n), max(t) FROM b67_g GROUP BY ROLLUP (g, k) ORDER BY 1, 2
SELECT g, k, count(*) FROM b67_g GROUP BY CUBE (g, k) ORDER BY 1, 2, 3
SELECT g, k % 2, count(*), min(f), max(f) FROM b67_g GROUP BY GROUPING SETS ((g), (k % 2), ()) ORDER BY 1, 2, 3
SELECT g, sum(f), avg(f) FROM b67_g GROUP BY ROLLUP (g) ORDER BY 1, 2
SELECT g, count(DISTINCT k), count(k) FILTER (WHERE id > 100) FROM b67_g GROUP BY ROLLUP (g) ORDER BY 1, 2, 3
SELECT g, k, GROUPING(g, k), count(*) FROM b67_g GROUP BY CUBE (g, k) ORDER BY 1, 2, 3
SELECT j, count(*) FROM b67_g GROUP BY ROLLUP (j) ORDER BY 1, 2
SELECT g, count(*) FROM b67_g WHERE id > 200 GROUP BY GROUPING SETS ((), (g)) ORDER BY 1
SELECT g, count(*) FROM b67_g WHERE id > 9999 GROUP BY GROUPING SETS ((), (g)) ORDER BY 1
SELECT g, sum(id * 2), max(upper(t)) FROM b67_g GROUP BY ROLLUP (g) ORDER BY 1, 2
SELECT d.name, b.g, count(*), sum(b.n) FROM b67_g b JOIN b67_d d ON b.k = d.k GROUP BY ROLLUP (d.name, b.g) ORDER BY 1, 2
SELECT d.name, count(*) FROM b67_g b LEFT JOIN b67_d d ON b.k = d.k GROUP BY CUBE (d.name) ORDER BY 1, 2
SELECT g, string_agg(t, ',' ORDER BY id DESC) FILTER (WHERE id < 20) FROM b67_g GROUP BY ROLLUP (g) ORDER BY 1
SELECT g, k, count(*) FROM b67_g GROUP BY GROUPING SETS ((g), (g), (k)) ORDER BY 1, 2, 3
SELECT g, bool_and(id > 0), bool_or(id > 399), array_agg(id ORDER BY id) FILTER (WHERE id < 5) FROM b67_g GROUP BY ROLLUP (g) ORDER BY 1
SELECT g, k, count(*) FROM b67_g GROUP BY ROLLUP (g, k) HAVING count(*) > 20 ORDER BY 1, 2
SELECT d.name, b.g, count(*) FROM b67_g b JOIN b67_d d ON b.k = d.k GROUP BY CUBE (d.name, b.g) ORDER BY 1, 2, 3
SELECT b.id, d.name FROM b67_d d JOIN b67_g b ON b.k = d.k WHERE b.id < 30 ORDER BY 1
SELECT count(*), sum(b.id) FROM b67_d d JOIN b67_g b ON b.k = d.k
SELECT count(*), sum(b.id) FROM b67_d d RIGHT JOIN b67_g b ON b.k = d.k
SELECT count(*), sum(b.id) FROM b67_d d FULL JOIN b67_g b ON b.k = d.k
