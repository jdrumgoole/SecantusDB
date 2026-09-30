SELECT id FROM wf_t WHERE lower(email) = 'a@x.com' ORDER BY id
SELECT count(*) FROM wf_t WHERE lower(email) = 'a@x.com'
SELECT count(*) FROM wf_t WHERE upper(email) LIKE 'A%'
SELECT sum(n) FROM wf_t WHERE length(email) > 1
SELECT email, count(*) FROM wf_t WHERE abs(n - 2) <= 1 GROUP BY email ORDER BY email
SELECT max(n) FROM wf_t WHERE n % 2 = 1
# An aggregate over a derived source whose WHERE is not a plain filter: the
# rows are filtered in a FROM subquery under the same name.
SELECT count(*) FROM (VALUES (1)) v(x) WHERE length('a' || chr(10)) > x
SELECT count(*) FROM (SELECT 1 AS x) s WHERE upper('a') = 'A'
SELECT count(*) FROM generate_series(1, 3) g WHERE upper('a') = 'A'
SELECT count(*) FROM (VALUES (1), (2)) v(x) WHERE x + 1 > 2
SELECT count(*) FROM (VALUES (1), (2)) v(x) WHERE abs(x) > 1
SELECT count(*) FROM (VALUES ('a'), ('b')) v(s) WHERE s || 'x' = 'ax'
SELECT count(*) FROM (VALUES (1), (2)) v(x) WHERE coalesce(x, 0) = 2
SELECT count(*) FROM (VALUES (1), (2)) v(x) WHERE CASE WHEN x = 1 THEN true END
SELECT sum(x), max(v.x) FROM (VALUES (1), (2), (3)) v(x) WHERE x % 2 = 1
SELECT g % 2, count(*) FROM generate_series(1, 6) g WHERE g * 2 > 4 GROUP BY 1 ORDER BY 1
