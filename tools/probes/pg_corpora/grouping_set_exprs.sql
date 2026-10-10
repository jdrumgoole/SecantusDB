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
# A HAVING, select-list or ORDER BY expression over the keys, and over an expression key: grouped in an inner query, computed in an outer one.
SELECT g, count(*) FROM gse_t GROUP BY ROLLUP (g) HAVING g || 'x' = 'ax'
SELECT upper(g) || 'x' FROM gse_t GROUP BY ROLLUP (upper(g)) ORDER BY 1
SELECT g, count(*) FROM gse_t GROUP BY ROLLUP (g) HAVING g = 'a'
SELECT g, count(*) FROM gse_t GROUP BY CUBE (g) HAVING g IS NULL ORDER BY 2
SELECT g, count(*) FROM gse_t GROUP BY CUBE (g, z) HAVING coalesce(g, z) = 'a' OR length(z) > 5 ORDER BY 1, 2
SELECT g, z, sum(v), grouping(g) + 10, grouping(g, z) FROM gse_t GROUP BY GROUPING SETS ((g, z), (g), ()) HAVING sum(v) + 0 > 2 ORDER BY 1, 2, 3
SELECT upper(g), length(upper(g)), count(*) FROM gse_t GROUP BY CUBE (upper(g), v % 2) HAVING length(upper(g)) = 1 ORDER BY 1, 3
SELECT upper(g) || (v % 2)::text, count(*) FROM gse_t GROUP BY ROLLUP (upper(g), v % 2) ORDER BY 1, 2
SELECT coalesce(g, 'all'), count(*) FROM gse_t GROUP BY ROLLUP (g) ORDER BY count(*), 1
SELECT g, count(*) FROM gse_t GROUP BY ROLLUP (g) ORDER BY g || 'x' DESC, 2
SELECT DISTINCT g IS NULL FROM gse_t GROUP BY CUBE (g, z) ORDER BY 1
SELECT CASE WHEN grouping(g) = 1 THEN 'total' ELSE g END, sum(v) FROM gse_t GROUP BY ROLLUP (g) ORDER BY 2
SELECT g, (SELECT 1) FROM gse_t GROUP BY ROLLUP (g) ORDER BY 1
SELECT g, (SELECT max(v) FROM gse_t i WHERE i.g = o.g) FROM gse_t o GROUP BY ROLLUP (g) ORDER BY 1
SELECT z || 'x' FROM gse_t GROUP BY ROLLUP (g)
SELECT g FROM gse_t GROUP BY ROLLUP (upper(g))
SELECT g, count(*) FROM gse_t GROUP BY (g, z) ORDER BY 1, 2
SELECT g, z FROM gse_t GROUP BY (g, z), v ORDER BY 1, 2
SELECT count(*) FROM gse_t GROUP BY ROW(g, z) ORDER BY 1
# Still different here, and listed so the day they agree is noticed: a parenthesised step inside ROLLUP / CUBE, and an unqualified table's own name used from a subquery.
SELECT g, z, count(*) FROM gse_t GROUP BY ROLLUP ((g, z), v) ORDER BY 1, 2, 3
SELECT g, (SELECT max(v) FROM gse_t i WHERE i.g = gse_t.g) FROM gse_t GROUP BY g ORDER BY 1
# The same error with a different name in it: PostgreSQL writes `gse_t.z`.
SELECT g, count(*) FROM gse_t GROUP BY ROLLUP (g) HAVING z = 'p'
