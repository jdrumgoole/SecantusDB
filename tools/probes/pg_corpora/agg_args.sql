SELECT sum(coalesce(n,0)) FROM ag13
SELECT max(coalesce(s,'-')) FROM ag13
SELECT string_agg(coalesce(s,'-'), ',' ORDER BY id) FROM ag13
SELECT array_agg(coalesce(s,'-') ORDER BY id) FROM ag13
SELECT array_agg(upper(s) ORDER BY id) FROM ag13
SELECT string_agg(s || '!', ',' ORDER BY id) FROM ag13
SELECT array_agg(length(s) ORDER BY id) FROM ag13
SELECT sum(abs(n)) FROM ag13
SELECT sum(round(x)) FROM ag13
SELECT g, string_agg(coalesce(s,'-'), ',' ORDER BY id) FROM ag13 GROUP BY g ORDER BY g
SELECT g, array_agg(upper(coalesce(s,'z')) ORDER BY id) FROM ag13 GROUP BY g ORDER BY g
SELECT min(lower(g)) FROM ag13
SELECT count(coalesce(s,'-')) FROM ag13
SELECT avg(coalesce(n,0)) FROM ag13
SELECT string_agg(id::text, '-' ORDER BY id) FROM ag13
SELECT array_agg(n * 2 ORDER BY id) FROM ag13
