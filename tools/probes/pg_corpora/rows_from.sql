# WITH ORDINALITY and ROWS FROM, measured against PostgreSQL 14.
SELECT * FROM generate_series(1,2) WITH ORDINALITY AS t(v, n)
SELECT * FROM generate_series(1,3) WITH ORDINALITY
SELECT t.*, ordinality FROM unnest(ARRAY['a','b']) WITH ORDINALITY AS t
SELECT * FROM unnest(ARRAY['x','y','z']) WITH ORDINALITY AS u(v, i) WHERE i > 1 ORDER BY i DESC
SELECT * FROM ROWS FROM (generate_series(1,2), unnest(ARRAY['a','b','c'])) AS t(a, b)
SELECT * FROM ROWS FROM (generate_series(1,2), unnest(ARRAY['a','b','c']))
SELECT * FROM ROWS FROM (generate_series(1,3), generate_series(10,11)) WITH ORDINALITY AS t(x, y, n)
SELECT count(*), max(n) FROM unnest(ARRAY[5,6,7]) WITH ORDINALITY AS t(v, n)
SELECT r.id, u.v, u.n FROM rft r, LATERAL unnest(r.a) WITH ORDINALITY AS u(v, n) ORDER BY 1, 3
SELECT * FROM jsonb_array_elements_text('["p","q"]') WITH ORDINALITY AS j(e, n)
SELECT pg_typeof(n) FROM generate_series(1,1) WITH ORDINALITY AS t(v, n)
# --- one output name twice is still two columns
SELECT * FROM (SELECT 1 AS a, 2 AS a) s
SELECT * FROM (SELECT 1 AS a, 'x' AS a) s
SELECT * FROM unnest(ARRAY[1,2], ARRAY['a','b','c'])
# --- a bare VALUES takes ORDER BY / LIMIT / OFFSET
VALUES (1, 'a'), (2, 'b') ORDER BY 1 DESC
VALUES (1), (2), (3) ORDER BY 1 DESC LIMIT 2
VALUES (1), (2), (3) OFFSET 1
VALUES (3), (1), (2) ORDER BY column1
# --- several set-returning functions in one select list run in lockstep
SELECT unnest(ARRAY[1,2]), unnest(ARRAY[10,20,30])
SELECT generate_series(1,2), generate_series(1,3)
SELECT unnest(ARRAY[1,2]) AS a, 'x' AS b, generate_series(5,7) * 2 AS c
SELECT id, unnest(a), unnest(a || a) FROM rft ORDER BY id
SELECT * FROM ROWS FROM (generate_series(1,3), unnest(ARRAY['p','q'])) AS t(n, s) WHERE n > 1
