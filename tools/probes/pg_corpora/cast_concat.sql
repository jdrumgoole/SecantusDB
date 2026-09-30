# `||` renders each side as its ::text: a numeric keeps its scale, a float4
# its own digits, a timestamptz its zone; an unknown beside a timestamp or an
# interval is text, not that type.
SET timezone = 'UTC'
SELECT 'a' || n, n || 'b', 'a' || m, 'a' || f, 'a' || i, 'a' || d, 'a' || b FROM xcc_t
SELECT 'a' || t, 'a' || iv, 'a' || u, 'y' || 0.1::float4, t || '!', 'z' || tz, 'r' || r FROM xcc_t
SELECT 'x' || 1.50::numeric(5,2), '{3}' || array[1,2], array[1] || '{4}'
# A cast with a modifier rounds and truncates to it, over a column too.
SELECT r::numeric(5,1), m::numeric(5,1), i::numeric(5,1), s::varchar(3), s::char(2) FROM xcc_t
SELECT t::timestamp(0), t::timestamp(3)::text, tz::timestamptz(0), tz::timestamp(0), t::timestamptz(0) FROM xcc_t
SELECT '2020-01-01 10:00:00.555555'::timestamp(3), '1990-01-01 00:00:00.5'::timestamp(0), '1999-12-31 23:59:59.95+00'::timestamptz(1)
# ORDER BY a name two different output columns carry is 42702.
SELECT a, a FROM xcc_o ORDER BY a
SELECT a, b AS a FROM xcc_o ORDER BY a
SELECT a::int8, a::text FROM xcc_o ORDER BY a
SELECT a AS x, a AS x FROM xcc_o ORDER BY x
SELECT xcc_o.a, a FROM xcc_o ORDER BY a
SELECT a AS b, b FROM xcc_o ORDER BY b
DROP TABLE xcc_t
DROP TABLE xcc_o
