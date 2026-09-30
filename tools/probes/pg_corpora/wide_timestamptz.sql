# Wide-year and BC timestamptz in the session zone (a zone's LMT before its first
# transition), numeric and POSIX zone settings, and far-future DST.
set timezone = 'UTC'
select '10000-01-01 12:00'::timestamptz::text
select '1000-01-01 12:00+00 BC'::timestamptz::text
select '0044-03-15 12:00:00.5 BC'::timestamptz::text
set timezone = 'Europe/Rome'
select '10000-01-01 12:00'::timestamptz::text
select '1000-01-01 12:00+00 BC'::timestamptz::text
select '0044-03-15 12:00:00.5 BC'::timestamptz::text
select '1000-01-01 12:00+02 BC'::timestamptz::text
set timezone = 'America/New_York'
select '1000-01-01 12:00 BC'::timestamptz::text
select '20000-06-15 08:00:00+05'::timestamptz::text
set timezone = '+03'
select '1000-01-01 12:00+00 BC'::timestamptz::text
set timezone = 'America/New_York'
select '2050-06-15 08:00:00+05'::timestamptz::text
select '2300-06-15 08:00:00+05'::timestamptz::text
select '9999-06-15 08:00:00+05'::timestamptz::text
set timezone = '+03'
select '2020-01-01 12:00+00'::timestamptz::text
set timezone = 'UTC+3'
select '2020-01-01 12:00+00'::timestamptz::text
SET timezone = 'Europe/Rome'
INSERT INTO wb VALUES (1, '1000-01-01 12:00 BC'), (2, '10000-01-01 12:00+00'), (3, '0044-03-15 12:00:00.5 BC')
SELECT id, t::text FROM wb ORDER BY id
SELECT id, extract(epoch from t) FROM wb ORDER BY id
SET timezone = 3
SELECT current_setting('timezone'), '2020-06-01 12:00+00'::timestamptz::text
SET timezone = -3
SELECT current_setting('timezone'), '2020-06-01 12:00+00'::timestamptz::text
SET timezone = 'GMT-2'
SELECT '2020-06-01 12:00+00'::timestamptz::text
SET timezone = '<+04>-04'
SELECT '2020-06-01 12:00+00'::timestamptz::text
SET timezone = 'UTC'
