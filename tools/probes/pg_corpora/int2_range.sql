# reference-version: 15
# A smallint holds -32768..32767: a value outside it is 22003 on a cast and on
# a write. Nothing checked it, so `40000::smallint` answered 40000 and a
# smallint column stored it.
SELECT 40000::smallint
SELECT (-40000)::smallint
SELECT 32767::smallint
SELECT (-32768)::smallint
SELECT 32768::smallint
SELECT 40000::int4::int2
SELECT 40000::int8::int2
SELECT '40000'::smallint
SELECT '32767'::smallint
SELECT ' -32768 '::smallint
SELECT '-32769'::smallint
SELECT 'x'::smallint
SELECT ''::smallint
SELECT '99999999999'::int
SELECT '-99999999999'::int
SELECT '2147483647'::int
SELECT '12x'::int
SELECT 40000.4::float8::smallint
SELECT 32767.4::float8::smallint
SELECT 32767.5::float8::smallint
SELECT 3e9::float8::int
SELECT (-3e9)::float8::int
SELECT 2147483647.4::float8::int
SELECT 'NaN'::float8::int
SELECT 'Infinity'::float8::int
SELECT 'NaN'::float8::smallint
SELECT 40000::numeric::smallint
SELECT ARRAY[1, 40000]::smallint[]
INSERT INTO i2r VALUES (2, 40000)
INSERT INTO i2r VALUES (3, 32767)
INSERT INTO i2r VALUES (4, '40000')
INSERT INTO i2r VALUES (5, 40000.0)
UPDATE i2r SET n = 40000 WHERE k = 1
UPDATE i2r SET n = n + 32767 WHERE k = 1
UPDATE i2r SET n = -32768 WHERE k = 1
SELECT k, n FROM i2r ORDER BY k
