# reference-version: 15
# timestamptz assignments keep the instant (a column copied, a bound
# parameter, an array parameter) in a summer-time zone; fractional seconds
# round to the microsecond; `+hhmm` offsets; timetz + interval; an untyped
# operand beside an interval; (timestamptz + interval) - timestamptz.
SET TimeZone = 'Europe/Dublin'
INSERT INTO b39_tz (id, a) VALUES (1, '1997-08-16 23:51-04')
UPDATE b39_tz SET b = a, c = a, d = a WHERE id = 1
SELECT a, b, c, d FROM b39_tz WHERE id = 1
INSERT INTO b39_tz (id, a, c) VALUES (2, %s, %s) ||| [datetime.datetime(1997, 8, 16, 20, 51, tzinfo=datetime.timezone.utc), datetime.datetime(1997, 8, 16, 20, 51, tzinfo=datetime.timezone.utc)]
SELECT a, c FROM b39_tz WHERE id = 2
UPDATE b39_tz SET b = %s WHERE id = 2 ||| [datetime.datetime(1997, 8, 16, 20, 51, tzinfo=datetime.timezone.utc)]
SELECT b FROM b39_tz WHERE id = 2
INSERT INTO b39_tz (id, e) VALUES (3, '{"1996-01-23 12:00:00-08","1997-08-16 16:51:00-04",NULL}')
SELECT e, e::text, e::varchar FROM b39_tz WHERE id = 3
SELECT '2000-02-07 15:00:00.000000789'::timestamp, '2018-12-31 23:59:59.999999500'::timestamp, '2000-01-01 00:00:00.0000015'::timestamp
SELECT '2018-12-31 23:59:59.9999995+02'::timestamptz
SELECT '00:00:05.123456 +0300'::timetz, '1970-01-01 00:00:05.123456 +0300'::timestamptz, '00:00:05+0530'::timetz, '2001-01-01 10:00-0130'::timestamptz
SELECT '12:00:00+03'::timetz + '1 hour'::interval, '01:00+03'::timetz - '2 hours'::interval, '1 hour'::interval + '12:00+03'::timetz
SELECT %s + '1 hour'::interval ||| ['12:00:00']
SELECT '1 day' + '1 hour'::interval, '1 day' - '1 hour'::interval
SELECT ('2020-01-01 00:00+00'::timestamptz + interval '1.5 s') - '2020-01-01 00:00+00'::timestamptz
SELECT extract(epoch from ((CAST(3 || ' second' AS interval) + now()) - now()))
SELECT ('2020-01-01 10:00'::timestamp + interval '1 mon') - '2020-01-01 10:00'::timestamp
SET TimeZone = 'UTC'
DROP TABLE b39_tz
