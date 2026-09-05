# --- no rows at all
SELECT sum(n) FROM empt
SELECT avg(n) FROM empt
SELECT count(n) FROM empt
SELECT count(*) FROM empt
SELECT min(n), max(n) FROM empt
SELECT array_agg(n) FROM empt
SELECT string_agg(s, ',') FROM empt
SELECT bool_and(n > 0) FROM empt
SELECT bool_or(n > 0) FROM empt
SELECT sum(x) FROM empt
# --- a group whose values are ALL NULL
SELECT g, sum(n) FROM ag GROUP BY g ORDER BY g
SELECT g, sum(x) FROM ag GROUP BY g ORDER BY g
SELECT g, sum(f) FROM ag GROUP BY g ORDER BY g
SELECT g, avg(n) FROM ag GROUP BY g ORDER BY g
SELECT g, count(n) FROM ag GROUP BY g ORDER BY g
SELECT g, count(*) FROM ag GROUP BY g ORDER BY g
SELECT g, min(n), max(n) FROM ag GROUP BY g ORDER BY g
SELECT g, array_agg(n) FROM ag GROUP BY g ORDER BY g
SELECT g, string_agg(s, ',') FROM ag GROUP BY g ORDER BY g
SELECT g, bool_and(b) FROM ag GROUP BY g ORDER BY g
SELECT g, bool_or(b) FROM ag GROUP BY g ORDER BY g
# --- whole-table aggregate over all-NULL column
SELECT sum(n) FROM ag WHERE id = 3
SELECT sum(n) FROM ag WHERE id = 99
SELECT sum(n) + 0 FROM ag WHERE id = 99
SELECT coalesce(sum(n), -1) FROM ag WHERE id = 99
# --- FILTER that matches nothing
SELECT sum(n) FILTER (WHERE id > 99) FROM ag
SELECT count(n) FILTER (WHERE id > 99) FROM ag
# --- HAVING over the same
SELECT g FROM ag GROUP BY g HAVING sum(n) IS NULL ORDER BY g
# --- windowed sum over an empty frame
SELECT id, sum(n) OVER (ORDER BY id ROWS BETWEEN 5 PRECEDING AND 4 PRECEDING) FROM ag ORDER BY id
