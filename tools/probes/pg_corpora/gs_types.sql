SELECT * FROM generate_series('2020-01-01'::date, '2020-01-03'::date, '1 day') g
SELECT g::text FROM generate_series('2020-01-31'::timestamp, '2020-06-01'::timestamp, '1 month') g
SELECT count(*) FROM generate_series('2020-01-01 00:00'::timestamptz, '2020-01-01 03:00'::timestamptz, interval '30 minutes')
SELECT * FROM generate_series('2020-01-03'::timestamp, '2020-01-01'::timestamp, '-1 day')
SELECT * FROM generate_series('2020-01-01'::timestamp, '2020-01-03'::timestamp, '0 day')
SELECT g FROM generate_series(1.5, 3, 0.5) g
SELECT g::text FROM generate_series(0.1, 0.35, 0.1) g
SELECT * FROM generate_series(1::numeric, 2::numeric)
SELECT * FROM generate_series(3.0, 1.0, -0.75)
SELECT * FROM generate_series(1.0, 2.0, 0)
SELECT * FROM generate_series('NaN'::numeric, 2.0)
SELECT * FROM generate_series(1.0, NULL)
SELECT pg_typeof(g) FROM generate_series('2020-01-01'::date, '2020-01-01'::date, '1 day') g
SELECT pg_typeof(g) FROM generate_series(1.5, 2) g
SELECT x FROM generate_series('2020-01-01'::timestamp, '2020-01-02'::timestamp, '12 hours') AS t(x) ORDER BY x DESC
