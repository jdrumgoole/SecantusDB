# Bare time input forms, the German DateStyle, and to_date's zero fields.
SELECT '12:34 pm'::time
SELECT '12:34 am'::time
SELECT 'allballs'::time
SELECT 'T12:34'::time
SELECT '1:2:3.5 am'::time
SELECT '040506.789'::time
SELECT '040506'::time
SELECT '12:34 pm'::timetz
SELECT 'allballs'::timetz
SELECT to_date('2020-00-10', 'YYYY-MM-DD')
SELECT to_date('2020-01-00', 'YYYY-MM-DD')
SET DateStyle = 'German'
SELECT '2020-03-04'::date::text
SELECT ARRAY['2020-03-04'::date, '2021-12-31'::date]::text
SELECT '2020-03-04 05:06:07'::timestamp::text
SELECT d::text, ts::text, a::text FROM tid
SELECT d::varchar FROM tid
SELECT ARRAY['2020-03-04'::date]
SELECT a FROM tid
SET DateStyle = 'SQL, DMY'
SELECT d::text, ts::text, a::text FROM tid
SET DateStyle = 'Postgres, MDY'
SELECT d::text, ts::text, a::text FROM tid
SET DateStyle = 'ISO, MDY'
SET TimeZone = 'UTC'
SELECT '0044-03-15 12:00:00 BC'::timestamptz::text
SELECT '1800-01-01 00:00:00'::timestamptz::text
SET TimeZone = 'Europe/Moscow'
SELECT '1800-01-01 00:00:00'::timestamptz::text
SELECT '2020-01-01 12:00 MSK'::timestamptz::text
SET TimeZone = 'UTC'
SELECT '2020-01-01 12:00 MSK'::timestamptz::text
SELECT 'a'::text * 2
DROP AGGREGATE no_such_agg(int)
DROP AGGREGATE IF EXISTS no_such_agg(int)
UPDATE ti SET n = 'x'::text
UPDATE ti SET n = n::text
