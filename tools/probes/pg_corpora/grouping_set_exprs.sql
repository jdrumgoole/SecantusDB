# reference-version: 15
# A select-list expression over the keys of GROUPING SETS / ROLLUP / CUBE, with no aggregate in it, and the queries that wrap one (an outer aggregate prunes the inner list to a constant).
SELECT 1 FROM gse_t GROUP BY CUBE (g, z)
SELECT 1 AS one, 'x' FROM gse_t GROUP BY ROLLUP (g)
SELECT NULL FROM gse_t GROUP BY GROUPING SETS ((g), ())
SELECT 2, g FROM gse_t GROUP BY CUBE (g) ORDER BY 2
SELECT g || 'x' FROM gse_t GROUP BY CUBE (g) ORDER BY 1
SELECT g || 'x', count(*) FROM gse_t GROUP BY CUBE (g) ORDER BY 1, 2
SELECT coalesce(g, '-') || coalesce(z, '-') FROM gse_t GROUP BY CUBE (g, z) ORDER BY 1
SELECT upper(g) FROM gse_t GROUP BY GROUPING SETS ((g), (z)) ORDER BY 1
SELECT gse_t.g || 'x', length(g) FROM gse_t GROUP BY ROLLUP (gse_t.g) ORDER BY 1
SELECT q.g || 'y' FROM gse_t q GROUP BY ROLLUP (q.g) ORDER BY 1
SELECT g, GROUPING(g), 7 FROM gse_t GROUP BY ROLLUP (g) ORDER BY 1, 2
SELECT CASE WHEN g IS NULL THEN 'T' ELSE g END, sum(v) FROM gse_t GROUP BY ROLLUP (g) ORDER BY 2, 1
SELECT (SELECT 1), g FROM gse_t GROUP BY CUBE (g) ORDER BY 2
SELECT v + 1 FROM gse_t GROUP BY CUBE (g)
SELECT v + count(*) FROM gse_t GROUP BY CUBE (g)
SELECT z FROM gse_t GROUP BY ROLLUP (g)
SELECT count(*) FROM (SELECT g, z, sum(v) s FROM gse_t GROUP BY CUBE (g, z)) q
SELECT max(s), min(s) FROM (SELECT g, z, sum(v) s FROM gse_t GROUP BY CUBE (g, z)) q
SELECT g, count(*) FROM (SELECT g, z, sum(v) s FROM gse_t GROUP BY CUBE (g, z)) q GROUP BY g ORDER BY 1
SELECT g, (SELECT count(*) FROM (SELECT z FROM gse_t GROUP BY ROLLUP (z)) r) FROM gse_t ORDER BY k
WITH q AS (SELECT g, z, sum(v) s FROM gse_t GROUP BY CUBE (g, z)) SELECT count(*) FROM q
SELECT count(*) FROM (SELECT g FROM gse_t GROUP BY GROUPING SETS ((g), ())) q
SELECT EXISTS (SELECT 1 FROM gse_t GROUP BY ROLLUP (g) HAVING count(*) > 3)
# Still refused here (0A000), and listed so the day they work is noticed:
SELECT g, count(*) FROM gse_t GROUP BY ROLLUP (g) HAVING g || 'x' = 'ax'
SELECT upper(g) || 'x' FROM gse_t GROUP BY ROLLUP (upper(g)) ORDER BY 1
