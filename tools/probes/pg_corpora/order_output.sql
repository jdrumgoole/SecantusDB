SELECT n::text FROM ob_t ORDER BY 1
SELECT n::text AS s FROM ob_t ORDER BY s
SELECT n::text FROM ob_t ORDER BY n::text
SELECT n::text, id FROM ob_t ORDER BY 1 DESC
SELECT a.n::text FROM ob_t a, ob_t b WHERE a.id = b.id ORDER BY 1
SELECT -n FROM ob_t ORDER BY 1
SELECT n * -1 AS m FROM ob_t ORDER BY m
SELECT length(n::text) * -1 + id AS m FROM ob_t ORDER BY 1
SELECT md5(n::text) FROM ob_t ORDER BY 1
SELECT coalesce(n::text, 'x') FROM ob_t ORDER BY 1
SELECT n::text FROM ob_t GROUP BY n ORDER BY 1
SELECT DISTINCT n::text FROM ob_t ORDER BY 1
SELECT n::text FROM ob_t UNION SELECT '5' ORDER BY 1
SELECT n::text, row_number() OVER () FROM ob_t ORDER BY 1
SELECT n::text FROM ob_t GROUP BY n ORDER BY n
SELECT n + 1 FROM ob_t GROUP BY n ORDER BY n
SELECT n::text, count(*) FROM ob_t GROUP BY n ORDER BY n
SELECT -n, count(*) FROM ob_t GROUP BY n ORDER BY 1
SELECT n::text AS s, count(*) FROM ob_t GROUP BY n ORDER BY s
SELECT upper(n::text) FROM ob_t GROUP BY n ORDER BY 1
SELECT n + id FROM ob_t GROUP BY n
SELECT n + 1, count(*) FROM ob_t GROUP BY n HAVING count(*) > 0 ORDER BY 1 DESC
SELECT n % 2 AS parity, sum(n) FROM ob_t GROUP BY n % 2 ORDER BY 1
SELECT (n % 2)::text AS parity, sum(n) FROM ob_t GROUP BY n % 2 ORDER BY parity
SELECT coalesce(max(n), 0) + 1 FROM ob_t
SELECT n::text || '!' FROM ob_t GROUP BY n ORDER BY 1 LIMIT 2
SELECT -n AS n FROM ob_t ORDER BY n
SELECT n AS x, -n AS n FROM ob_t ORDER BY n
SELECT n::text FROM ob_t ORDER BY 1 LIMIT 1 OFFSET 1
SELECT DISTINCT -n FROM ob_t ORDER BY 1
