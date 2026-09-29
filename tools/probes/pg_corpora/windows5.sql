SELECT g, sum(v), sum(sum(v)) OVER (ORDER BY g) FROM wv5 GROUP BY g ORDER BY g
SELECT g, count(*), rank() OVER (ORDER BY count(*) DESC, g) FROM wv5 GROUP BY g ORDER BY g
SELECT g, sum(v)::numeric / sum(sum(v)) OVER () AS share FROM wv5 GROUP BY g ORDER BY g
SELECT g, h, sum(v), sum(sum(v)) OVER (PARTITION BY g ORDER BY h) FROM wv5 GROUP BY g, h ORDER BY g, h
SELECT g, max(v), row_number() OVER (ORDER BY max(v) DESC NULLS LAST) FROM wv5 GROUP BY g ORDER BY 3
SELECT g, sum(v), lag(sum(v)) OVER (ORDER BY g) FROM wv5 GROUP BY g HAVING count(*) > 1 ORDER BY g
SELECT count(*) OVER () FROM wv5 GROUP BY g ORDER BY 1
SELECT h, count(*), avg(count(*)) OVER () FROM wv5 GROUP BY h ORDER BY h
SELECT k.label, sum(w.v), dense_rank() OVER (ORDER BY sum(w.v) DESC) FROM wv5 w JOIN wv5k k ON k.g = w.g GROUP BY k.label ORDER BY 1
SELECT g, sum(v) AS total, sum(sum(v)) OVER w FROM wv5 GROUP BY g WINDOW w AS (ORDER BY g) ORDER BY g
SELECT n, row_number() OVER (ORDER BY n DESC) FROM generate_series(1, 4) AS t(n) ORDER BY n
SELECT n, sum(n) OVER (ORDER BY n) FROM generate_series(1, 5) n ORDER BY n
SELECT n, lead(n) OVER () FROM generate_series(10, 30, 10) AS s(n) ORDER BY n
SELECT id, v::numeric / sum(v) OVER () FROM wv5 WHERE v IS NOT NULL ORDER BY id
SELECT id, v - avg(v) OVER (PARTITION BY g) FROM wv5 ORDER BY id
SELECT id, row_number() OVER (ORDER BY id) * 10 + 1 FROM wv5 ORDER BY id
SELECT id, coalesce(lag(v) OVER (ORDER BY id), 0) FROM wv5 ORDER BY id
