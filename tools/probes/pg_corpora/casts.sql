# --- casts and coercion edges not yet swept
SELECT '5'::int + 1, '5'::int8, ' 5'::int
SELECT 'true'::bool, 't'::bool, 'y'::bool, '1'::bool, 'no'::bool
SELECT 1.9::int, -1.9::int, 1.5::int, 2.5::int, 0.5::int
SELECT 'abc'::char(2), 'a'::char(3) || '|', 'abc'::varchar(2)
SELECT 12345::text, 1.50::text, true::text, NULL::text
SELECT '2020-1-5'::date, '20200105'::date, 'Jan 5, 2020'::date
SELECT '{1,2}'::int[], '{}'::int[], '{NULL}'::int[]
SELECT 1::float4::text, 1.1::float4::float8::text
SELECT (1/3.0)::float4
SELECT 'x'::text::bytea
SELECT '\x41'::bytea, 'A'::bytea
SELECT 1::bit(4), b'1010'::int
SELECT '00000000-0000-0000-0000-000000000001'::uuid
SELECT '1 day'::interval, '1'::interval
SELECT 123::char(2)
SELECT 'abc'::"char"
SELECT null::int + 1, null::text || 'x'
SELECT x::int, n::text, d::text, b::text FROM p15 ORDER BY id
SELECT CAST(n AS text) FROM p15 ORDER BY id
SELECT pg_typeof('a'), pg_typeof('a'::text), pg_typeof(1), pg_typeof(1.0)
