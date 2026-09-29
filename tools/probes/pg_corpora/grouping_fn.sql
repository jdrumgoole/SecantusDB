SELECT a, b, sum(n), grouping(a), grouping(b), grouping(a, b) FROM gf_t GROUP BY ROLLUP (a, b) ORDER BY a NULLS LAST, b NULLS LAST
SELECT a, grouping(a) AS g, count(*) FROM gf_t GROUP BY CUBE (a) ORDER BY g, a NULLS LAST
SELECT a, b, grouping(b, a) FROM gf_t GROUP BY GROUPING SETS ((a), (b), ()) ORDER BY 3, a NULLS LAST, b NULLS LAST
SELECT a, grouping(a) FROM gf_t GROUP BY a ORDER BY a NULLS LAST
SELECT a, grouping(n) FROM gf_t GROUP BY a
SELECT grouping(a) FROM gf_t
SELECT a, count(*) FROM gf_t GROUP BY a ORDER BY count(*) DESC, a NULLS LAST
SELECT a, count(*) AS c FROM gf_t GROUP BY a ORDER BY c, a NULLS LAST
SELECT a, count(*) FROM gf_t GROUP BY a ORDER BY 2, 1 NULLS LAST
SELECT a, sum(n) FROM gf_t GROUP BY ROLLUP (a) ORDER BY 2, 1 NULLS FIRST
SELECT a, count(*) FROM gf_t GROUP BY a ORDER BY sum(n) DESC, a NULLS FIRST
SELECT a, count(*), count(n) FROM gf_t GROUP BY a ORDER BY count(*) DESC, a NULLS LAST LIMIT 2
SELECT b, max(n) - min(n) AS spread FROM gf_t GROUP BY b ORDER BY spread DESC, b
SELECT a, count(*) FROM gf_t GROUP BY a HAVING count(*) > 0 ORDER BY count(*) DESC, a NULLS LAST OFFSET 1
SELECT a, count(*) FROM gf_t GROUP BY a ORDER BY 3
