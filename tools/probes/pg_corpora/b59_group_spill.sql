# GROUP BY in bounded memory (batch 59): past a byte budget the rows are
# sorted on the group key and grouped one group at a time. Every query has
# an ORDER BY, so the answers compare whatever path ran; run with
# SECANTUS_PG_GROUP_MEMORY_BYTES=1000 to force the sorted path.
SELECT k, count(*), sum(id), min(t), max(n) FROM b59_g GROUP BY k ORDER BY k
SELECT t, k, count(*) FROM b59_g GROUP BY t, k ORDER BY t, k NULLS FIRST
SELECT n, count(*), avg(id) FROM b59_g GROUP BY n ORDER BY n
SELECT f, count(*) FROM b59_g GROUP BY f ORDER BY f
SELECT k % 5 AS m, count(DISTINCT t), string_agg(DISTINCT t, ',' ORDER BY t) FROM b59_g GROUP BY k % 5 ORDER BY m
SELECT length(t) + k AS e, count(*) FROM b59_g WHERE id > 100 GROUP BY 1 ORDER BY 1
SELECT k, count(*) FROM b59_g GROUP BY k HAVING count(*) > 80 ORDER BY 2 DESC, 1
SELECT k, array_agg(id ORDER BY id DESC) FILTER (WHERE id < 60) FROM b59_g GROUP BY k ORDER BY k LIMIT 5
SELECT k, sum(id) FROM b59_g GROUP BY k ORDER BY sum(id) DESC OFFSET 3 LIMIT 4
SELECT count(*) FROM (SELECT t, n FROM b59_g GROUP BY t, n) s
SELECT k, count(*) FROM b59_g WHERE t > 'r3' GROUP BY k ORDER BY k
SELECT k / 0, count(*) FROM b59_g GROUP BY 1
