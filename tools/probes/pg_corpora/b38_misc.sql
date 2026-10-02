# reference-version: 15
# Small PostgreSQL 15 features pgjdbc reaches for: encoding functions,
# negative numeric scale, DateStyle keywords, interval + datetime, the
# timestamp -> time assignment casts, a varbit domain, inet in pg_type.
SELECT getdatabaseencoding()
SELECT pg_encoding_to_char(6), pg_char_to_encoding('UTF8'), pg_char_to_encoding('utf-8'), pg_char_to_encoding('nope'), pg_encoding_to_char(99)
SELECT 1.5::numeric(5,-1), 125::numeric(5,-1), 123.456::numeric(4,-2)
SELECT 12345.678::numeric(3,-2), -12345.678::numeric(3,-2)
SELECT 999950::numeric(3,-3)
INSERT INTO b38_ns VALUES (12345), (-49)
SELECT n FROM b38_ns ORDER BY n
INSERT INTO b38_ns VALUES (100000)
SET datestyle = 'PostgreSQL'
SHOW datestyle
SET datestyle = 'German'
SHOW datestyle
SET datestyle = 'euro'
SHOW datestyle
SET datestyle = 'ISO, US'
SHOW datestyle
SET datestyle = 'ISO, MDY'
SELECT (CAST(3 || ' second' AS interval) + '2020-01-01 00:00:00+00'::timestamptz)::text
SELECT interval '1 day' + '2020-01-01'::timestamp, interval '1 day' + date '2020-01-01', interval '1 hour' + time '10:00'
SET timezone = 'UTC'
INSERT INTO b38_dt VALUES ('2020-01-01 10:11:12+02'::timestamptz, '2020-01-01 10:11:12+02'::timestamptz)
INSERT INTO b38_dt VALUES ('2020-01-01 10:11:12'::timestamp, NULL)
SELECT t::text, tz::text FROM b38_dt ORDER BY t
INSERT INTO b38_dt VALUES (NULL, '2020-01-01 10:11:12'::timestamp)
CREATE DOMAIN b38_vb AS varbit(3)
CREATE TABLE b38_dom (v b38_vb)
INSERT INTO b38_dom VALUES (B'101')
SELECT v::text FROM b38_dom
SELECT oid, typname FROM pg_type WHERE typname IN ('inet', 'cidr', 'money', 'refcursor', 'varbit', 'bit') ORDER BY oid
DROP TABLE b38_dom
DROP DOMAIN b38_vb
DROP TABLE b38_dt
DROP TABLE b38_ns
