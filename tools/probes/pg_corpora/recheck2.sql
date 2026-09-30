SELECT '[1,5)'::int4range @> 3, '[1,5)'::int4range && '[4,8)'::int4range, '[1,5)'::int4range -|- '[5,8)'::int4range
SELECT '[1,5)'::int4range <@ '[0,9)'::int4range, '[1,5)'::int4range @> '[2,3)'::int4range
SELECT ('{[1,3),[5,7)}'::int4multirange + '{[2,6)}'::int4multirange)::text
SELECT ('[1,5)'::int4range + '[3,8)'::int4range)::text, ('[1,5)'::int4range * '[3,8)'::int4range)::text, ('[1,5)'::int4range - '[3,8)'::int4range)::text
SELECT 42::oid, '42'::oid, pg_typeof(42::oid)
SELECT array[1, '2'], array['2020-01-01'::date, '2000-01-01']::text
SELECT array[1, 'a']
INSERT INTO rc2_j (data) VALUES (42)
INSERT INTO rc2_j (data) VALUES ('{}'::text)
INSERT INTO rc2_j (data) VALUES ('{}')
SET timezone = 'Europe/Rome'
SELECT tstzrange('2020-01-01 00:00+00','2020-06-01 10:00+00')::text
SELECT array['2020-01-01 12:00'::timestamptz]::text
SET timezone = 'UTC'
SELECT pg_typeof(user), pg_typeof(current_date), current_date = current_date
SELECT localtime IS NOT NULL, localtimestamp IS NOT NULL, current_time IS NOT NULL
BEGIN
INSERT INTO rc2_s (v) VALUES (1)
ROLLBACK
INSERT INTO rc2_s (v) VALUES (2) RETURNING id
CREATE TABLE rc2_bad (n int DEFAULT now())
SELECT array[1,2]::int4[] = array[1,2]::int2[]
SELECT array[1,2]::int4[] @> array[1]::int8[]
SELECT array[1,2] @> '{1}'
SELECT array[1.5] = array[1]
SELECT array['a'] = array['a']::varchar[]
SELECT array[1,2] && array[2]
SELECT array['a','b']::text[] <@ array['a','b','c']
SELECT array[1, '2']
SELECT array['2020-01-01'::date, '2000-01-01']::text
SELECT array[1, 'a']
SELECT array[1, 2.5]
SELECT array['a', 'b']
SELECT array[null, 1]
SELECT pg_typeof(array[1, '2'])
SELECT pg_typeof(array['x', 'y'])
SELECT array[[1,2],['3','4']]
SELECT array[1::int8, 2]
SELECT pg_typeof(array[1::int8, 2])
