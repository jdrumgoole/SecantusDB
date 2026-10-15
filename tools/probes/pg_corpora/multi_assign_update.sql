# reference-version: 15
# UPDATE ... SET (a, b) = (SELECT ...): each listed column reads one column of the subquery's single row (RP-031).
UPDATE mau_t SET (v, t) = (SELECT 5, 'z') WHERE k = 1 RETURNING *
UPDATE mau_t SET (v, t) = (SELECT n, s FROM mau_src WHERE mau_src.k = mau_t.k AND n < 201) WHERE k = 2 RETURNING *
SELECT * FROM mau_t ORDER BY k
UPDATE mau_t SET (v, t) = (SELECT n, s FROM mau_src WHERE mau_src.k = mau_t.k AND n < 201)
SELECT * FROM mau_t ORDER BY k
UPDATE mau_t SET (v, t) = (SELECT n, s FROM mau_src WHERE mau_src.k = mau_t.k)
UPDATE mau_t SET (v, t) = (SELECT n, s FROM mau_src WHERE mau_src.k = 9) WHERE k = 1 RETURNING *
UPDATE mau_t SET (v, t) = (SELECT 1) WHERE k = 1
UPDATE mau_t SET (v, t) = (SELECT 1, 'a', 2) WHERE k = 1
UPDATE mau_t SET (v, t) = (SELECT k + 1, c || '!') WHERE k = 2 RETURNING *
UPDATE mau_t SET (v, t) = (SELECT nextval('mau_seq'), currval('mau_seq')::text)
SELECT * FROM mau_t ORDER BY k
UPDATE mau_t SET (v, t, c) = (SELECT 7, 'q', 'abcd') WHERE k = 1
UPDATE mau_t SET (v, t) = (SELECT 'x', 'y') WHERE k = 1
UPDATE mau_t SET (v, t) = (SELECT '9', 9) WHERE k = 1
UPDATE mau_t SET c = 'w', (v, t) = (SELECT 3, 'three') WHERE k = 2 RETURNING *
UPDATE mau_t SET (v, t) = (SELECT 1, 'a' UNION ALL SELECT 2, 'b') WHERE k = 1
UPDATE mau_t SET (v, t) = (SELECT 1, 'a' UNION SELECT 1, 'a') WHERE k = 1 RETURNING *
UPDATE mau_t SET (v, t) = (SELECT * FROM (VALUES (4, 'four')) x) WHERE k = 1 RETURNING *
UPDATE mau_t SET (v, t) = (SELECT * FROM (VALUES (4, 'four', 1)) x) WHERE k = 1
UPDATE mau_t SET (v, t) = (SELECT n, s FROM mau_src ORDER BY 1 DESC LIMIT 1) WHERE k = 3 RETURNING *
UPDATE mau_t SET (v, v) = (SELECT 1, 2) WHERE k = 1
UPDATE mau_t SET v = 1, v = 2 WHERE k = 1
UPDATE mau_t SET (v, nope) = (SELECT 1, 2) WHERE k = 1
UPDATE mau_t SET (v, t) = (SELECT s2.n, s2.s) FROM mau_src s2 WHERE s2.k = mau_t.k AND s2.n = 100 RETURNING mau_t.v, mau_t.t
UPDATE mau_t SET (v, t) = (SELECT n, s FROM mau_src LIMIT 1) FROM mau_src s2 WHERE s2.k = mau_t.k AND s2.n = 100 RETURNING mau_t.v, mau_t.t
UPDATE mau_t SET (v) = (SELECT 42) WHERE k = 3 RETURNING *
UPDATE mau_t SET (v, t) = (SELECT max(n), min(s) FROM mau_src WHERE mau_src.k = mau_t.k)
SELECT * FROM mau_t ORDER BY k
UPDATE mau_t SET (v, t) = (1, 'one'), (c) = (SELECT 'zz') WHERE k = 1 RETURNING *
# A FROM-less subquery reading the updated row, and a row subquery that returns nothing.
UPDATE mau_t SET v = (SELECT v + 1) WHERE k = 1 RETURNING v
SELECT (SELECT row(n, s) FROM mau_src WHERE k = 9) IS NULL
# A record keeps a char(n) field's padding, and compares it without (RP-032).
SELECT (t)::text FROM mau_c t ORDER BY n
SELECT t FROM mau_c t ORDER BY n
SELECT row_to_json(t) FROM mau_c t ORDER BY n
SELECT array_agg(t ORDER BY n)::text FROM mau_c t
SELECT t = row('ab', 'ab', 1)::mau_c, t = row('ab ', 'ab', 1)::mau_c, t = row('ab   ', 'ab', 1)::mau_c FROM mau_c t WHERE n = 1
SELECT t FROM mau_c t WHERE t = row('ab', 'ab', 1)::mau_c
SELECT count(DISTINCT t) FROM mau_c t
SELECT (t).c, length((t).c), octet_length((t).c) FROM mau_c t ORDER BY n
SELECT row(t.*) FROM mau_c t ORDER BY n
SELECT row(t.*, 9)::text FROM mau_c t ORDER BY n
SELECT concat(t), format('%s', t), concat_ws('-', t, n) FROM mau_c t ORDER BY n
SELECT concat('x', row(1, 'a b'), 2), format('%s|%L|%I', row(1, 'a'), row(1, 'a'), row(1, 'a'))
# An outer column inside a correlated subquery has its own type there (it took the outer row's FIRST column's type).
SELECT (SELECT mau_t.c || '!' FROM mau_src LIMIT 1) FROM mau_t ORDER BY k
SELECT (SELECT pg_typeof(mau_t.c) FROM mau_src LIMIT 1) FROM mau_t ORDER BY k
SELECT (SELECT mau_t.c = 'x' FROM mau_src LIMIT 1) FROM mau_t ORDER BY k
SELECT k FROM mau_t WHERE EXISTS (SELECT 1 FROM mau_src WHERE mau_t.c = 'x') ORDER BY k
SELECT (SELECT pg_typeof(c)), (SELECT c = 'x'), (SELECT c::text), (SELECT length(c)) FROM mau_t ORDER BY k
SELECT (SELECT t || '-' || v) FROM mau_t ORDER BY k
