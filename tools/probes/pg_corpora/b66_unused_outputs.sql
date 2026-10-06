# reference-version: 15
# Batch 66: a FROM-subquery output nothing reads is never computed (remove_unused_subquery_outputs / pull-up).
SELECT count(*) FROM (SELECT a/b FROM b66_u) s
SELECT a FROM (SELECT a/b x, a FROM b66_u) s ORDER BY a
SELECT a FROM (SELECT a/b AS a2, a FROM b66_u) s ORDER BY 1
SELECT s.a FROM (SELECT a, a/b FROM b66_u) s ORDER BY 1
SELECT count(*) FROM (SELECT a/b AS q, t FROM b66_u) s WHERE t <> 'y'
SELECT count(*) FROM (SELECT (a/b)::int FROM b66_u) s
SELECT count(*) FROM (SELECT abs(a/b) FROM b66_u) s
SELECT count(*) FROM (SELECT CASE WHEN a > 0 THEN a/b END FROM b66_u) s
SELECT count(*) FROM (SELECT a/b FROM b66_u OFFSET 1) s
SELECT count(*) FROM (SELECT a/b FROM b66_u LIMIT 2) s
SELECT g, count(*) FROM (SELECT g, a/b AS q FROM b66_u) s GROUP BY g ORDER BY g
SELECT y FROM (SELECT a/b, a FROM b66_u) s(x, y) ORDER BY 1
SELECT count(*) FROM (SELECT a/b FROM b66_u) s, b66_u u
SELECT u.a FROM (SELECT a/b AS q, a FROM b66_u) s JOIN b66_u u ON u.a = s.a ORDER BY 1
SELECT count(*) FROM (SELECT count(*) AS n, sum(a/b) AS z FROM b66_u) s
SELECT n FROM (SELECT count(*) AS n, sum(a/b) AS z FROM b66_u) s
SELECT count(*) FROM (SELECT sum(a/b) FROM b66_u) s
SELECT count(*) FROM (SELECT g, sum(a/b) FROM b66_u GROUP BY g) s
SELECT count(*) FROM (SELECT a/b, row_number() OVER () FROM b66_u) s
SELECT count(*) FROM (SELECT (SELECT 1/0) FROM b66_u) s
SELECT count(*) FROM (SELECT count(*) FROM (SELECT a/b FROM b66_u) i) s
SELECT EXISTS (SELECT 1 FROM (SELECT a/b FROM b66_u) s)
SELECT count(*) FROM (SELECT a/b FROM b66_u UNION ALL SELECT 1) s
# used: the error stays
# an output nothing reads is still analysed: its errors stay
SELECT a FROM b66_u x, (SELECT x.a + 1 AS b) s
SELECT count(*) FROM (SELECT nosuch FROM b66_u) s
SELECT count(*) FROM (SELECT a, sum(b) FROM b66_u) s
SELECT count(*) FROM (SELECT 'a' + 1 FROM b66_u) s
SELECT count(*) FROM (SELECT nosuchfn(a) FROM b66_u) s
SELECT count(*) FROM (SELECT a/b AS x FROM b66_u) s, (SELECT a/b AS y FROM b66_u) t
SELECT * FROM (SELECT a/b FROM b66_u) s
SELECT s.* FROM (SELECT a/b FROM b66_u) s
SELECT count(s) FROM (SELECT a/b FROM b66_u) s
SELECT x FROM (SELECT a/b AS x FROM b66_u) s
SELECT count(*) FROM (SELECT a/b AS x FROM b66_u) s WHERE x > 0
SELECT count(*) FROM (SELECT a/b AS x FROM b66_u) s GROUP BY x
SELECT count(*) FROM (SELECT a/b AS x FROM b66_u ORDER BY x) s
SELECT count(*) FROM (SELECT a/b AS x FROM b66_u ORDER BY 1) s
SELECT count(*) FROM (SELECT a/b AS x FROM b66_u GROUP BY 1) s
SELECT count(*) FROM (SELECT DISTINCT a/b FROM b66_u) s
SELECT count(*) FROM (SELECT a/b AS x, a FROM b66_u) s JOIN b66_u u USING (x)
SELECT count(*) FROM (SELECT a/b AS a FROM b66_u) s NATURAL JOIN b66_u u
SELECT count(*) FROM (SELECT a/b FROM b66_u UNION ALL SELECT 1 FROM b66_u) s
# volatile and set-returning outputs are kept, and computed
SELECT count(*) FROM (SELECT nextval('b66_useq') FROM b66_u) s
SELECT currval('b66_useq')
SELECT count(*) FROM (SELECT a, generate_series(1, 3) FROM b66_u) s
SELECT count(*) FROM (SELECT a, unnest(ARRAY[1, 2]) FROM b66_u) s
SELECT count(*) FROM (SELECT a/b, random() FROM b66_u) s
# the outputs that ARE read keep their values
SELECT (SELECT s.x + max(u.a) FROM b66_u u) FROM (SELECT a * 2 AS x, a/b AS y FROM b66_u) s ORDER BY 1
SELECT x FROM (SELECT a AS x, a/b AS y FROM b66_u) s ORDER BY x
SELECT row_to_json(s)::text FROM (SELECT a, t FROM b66_u) s ORDER BY 1
SELECT count(*) FROM (SELECT a/b AS q FROM b66_u) s WHERE EXISTS (SELECT 1 FROM b66_u u WHERE u.a = s.q)
SELECT sum(x) OVER () FROM (SELECT a AS x, a/b AS y FROM b66_u) s
SELECT s.a, g FROM (SELECT a, a/b AS z FROM b66_u) s, LATERAL generate_series(1, s.a) g ORDER BY 1, 2
SELECT s.a, g FROM (SELECT a, a/b AS z FROM b66_u) s, LATERAL generate_series(1, z) g ORDER BY 1, 2
SELECT count(*) FROM (SELECT DISTINCT ON (g) g, a/b FROM b66_u ORDER BY g) s
SELECT x FROM (SELECT a AS x, sum(a/b) OVER () AS z FROM b66_u) s ORDER BY 1
SELECT count(*) FROM (SELECT a/b AS q FROM b66_u FOR UPDATE) s
SELECT x FROM (SELECT a AS x, a/b AS t FROM b66_u) s JOIN b66_u u ON u.t = 'x' ORDER BY 1
SELECT count(*) FROM (SELECT a, a/b AS t FROM b66_u) s ORDER BY count(*)
SELECT q FROM (SELECT a FROM b66_u) s, LATERAL (SELECT s.a/0 AS q, s.a AS r) l
WITH w AS (SELECT a/b FROM b66_u) SELECT count(*) FROM w
WITH w AS (SELECT a/b AS q, a FROM b66_u) SELECT a FROM w ORDER BY 1
WITH w AS MATERIALIZED (SELECT a/b FROM b66_u) SELECT count(*) FROM w
WITH w AS (SELECT a/b FROM b66_u) SELECT count(*) FROM w, w w2
WITH w AS (SELECT a/b AS q FROM b66_u) SELECT q FROM w
WITH w(q, r) AS (SELECT a/b, a FROM b66_u) SELECT r FROM w ORDER BY 1
WITH w AS (SELECT a/b, nextval('b66_useq') FROM b66_u) SELECT count(*) FROM w
