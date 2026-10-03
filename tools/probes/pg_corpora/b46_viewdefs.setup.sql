DROP TABLE IF EXISTS b46t, b46u CASCADE
CREATE TABLE b46t (a int, d date, ts timestamp, s text, n numeric)
CREATE TABLE b46u (a int, b text)
CREATE VIEW b46v1 AS SELECT to_char(d, 'YYYY') AS y, extract(year FROM d) AS e, date_part('year', d) AS dp FROM b46t
CREATE VIEW b46v2 AS SELECT extract(epoch FROM ts) AS e, extract(month FROM ts) + 1 AS m, to_char(a, '999') AS c, to_char(n, '99.9') AS cn, to_char(ts, 'HH24') AS ct FROM b46t
CREATE VIEW b46v3 AS SELECT round(a) AS r, left(s, a) AS l, date_trunc('day', d) AS dt, abs(a) AS ab FROM b46t
CREATE VIEW b46v4 AS SELECT b46t.a, b46u.b, (SELECT max(x.a) FROM b46t x WHERE x.a = b46t.a) AS mx, CASE WHEN b46t.a > 1 THEN 'big' ELSE 'small' END AS sz FROM b46t, b46u WHERE b46t.a = b46u.a
CREATE VIEW b46v5 AS SELECT x.a FROM b46t x, b46u y, b46t z, (SELECT a FROM b46u) w WHERE x.a = y.a
