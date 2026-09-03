# --- integer overflow / boundaries
SELECT a + 1 FROM t9
SELECT b + 1 FROM t9
SELECT f + 1 FROM t9
SELECT -a - 2 FROM t9
SELECT a * 2 FROM t9
SELECT abs(-2147483648)
SELECT (-2147483648)::int / (-1)
SELECT 2147483647::int + 1::bigint
SELECT 32767::smallint + 1
SELECT (9223372036854775807::bigint) * 2
# --- numeric scale/rounding
SELECT c, c * 2, c / 3, c + 0.0001 FROM t9
SELECT 1.005::numeric(10,2), 1.015::numeric(10,2), (-1.005)::numeric(10,2)
SELECT 10::numeric / 3, 1::numeric / 3, 2::numeric / 7
SELECT 100000::numeric / 3
SELECT round(1.5), round(2.5), round(-1.5), round(-2.5)
SELECT round(1.5::numeric), round(2.5::numeric)
SELECT round(1.5::float8::numeric)
SELECT 7 / 2, 7 % 2, (-7) / 2, (-7) % 2
SELECT 7.0 / 2, 7.0 % 2
SELECT 5 / 0.0
SELECT 0.0 / 5
SELECT 1e308 * 10
SELECT 'NaN'::numeric, 'NaN'::numeric + 1, 'NaN'::numeric = 'NaN'::numeric
SELECT 'Infinity'::float8 + 1, 'Infinity'::float8 - 'Infinity'::float8
SELECT d, e, d + e FROM t9
SELECT d::numeric, e::numeric FROM t9
SELECT 0.1::float8 + 0.2::float8
SELECT (0.1 + 0.2)::text
SELECT 1234567890123456789012345678901234567890::numeric
SELECT 1.23456789012345678901234567890123456789::numeric
SELECT trunc(1.9999, 3), round(1.9999, 3), ceil(1.0001), floor(1.9999)
SELECT scale(1.230), scale(1.2), min_scale(1.230), trim_scale(1.230)
SELECT numeric_send(1.5) IS NOT NULL
SELECT 3 ^ 2, 2 ^ 0.5, (-2) ^ 2
SELECT 10 % 3.5, 10.5 % 3
SELECT greatest(1, 2.5), least(1, 2.5)
SELECT 1::int2 + 1::int4 + 1::int8
SELECT pg_typeof(1 + 1), pg_typeof(1 + 1.0), pg_typeof(1::int8 + 1), pg_typeof(1.5::real + 1)
