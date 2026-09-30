SELECT x FROM eo_t ORDER BY x
SELECT x FROM eo_t ORDER BY x DESC
SELECT x FROM eo_t WHERE x > 'zeta' ORDER BY x
SELECT min(x), max(x) FROM eo_t
SELECT x FROM eo_t WHERE x < 'mid' ORDER BY id
SELECT 'zeta'::eo_m < 'alpha'::eo_m
SELECT x, row_number() OVER (ORDER BY x) FROM eo_t ORDER BY id
SELECT CASE WHEN count(*) > 1 THEN 'many' ELSE 'few' END FROM eo_t
SELECT x, count(*) FROM eo_t GROUP BY x ORDER BY x
SELECT x FROM eo_t ORDER BY 1
SELECT greatest('zeta'::eo_m, 'mid'::eo_m), least('zeta'::eo_m, 'mid'::eo_m)
SELECT x FROM eo_t WHERE x BETWEEN 'zeta' AND 'alpha' ORDER BY x
SELECT x FROM eo_t WHERE x > 'nope'
SELECT array_agg(x ORDER BY x) FROM eo_t
SELECT string_agg(x::text, ',' ORDER BY x DESC) FROM eo_t
SELECT array_agg(id ORDER BY id * -1) FROM eo_t
SELECT string_agg(x::text, ',' ORDER BY length(x::text), x) FROM eo_t
UPDATE eo_t SET id = id + 10 WHERE x > 'zeta'
SELECT id, x FROM eo_t ORDER BY id
DELETE FROM eo_t WHERE x < 'mid'
SELECT id, x FROM eo_t ORDER BY id
SELECT a.x FROM eo_t a JOIN eo_t b ON a.id = b.id ORDER BY a.x
SELECT count(*) FROM eo_t WHERE x >= ALL (SELECT x FROM eo_t)
