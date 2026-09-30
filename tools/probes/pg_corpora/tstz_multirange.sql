# tstzmultirange keeps its instants in UTC and renders them in the session zone.
SET timezone TO 'Europe/Rome'
SELECT tstzmultirange(tstzrange('2020-01-01 00:00+00','2020-06-01 10:00+00'))::text
SELECT tstzrange('2020-01-01 00:00+00','2020-06-01 10:00+00')::tstzmultirange::text
SELECT '{[2020-01-01 00:00+00,2020-06-01 10:00+00)}'::tstzmultirange::text
INSERT INTO tm VALUES (tstzmultirange(tstzrange('2020-01-01 00:00+00','2020-06-01 10:00+00')))
SELECT m::text FROM tm
SELECT tstzmultirange(tstzrange('2020-01-01 00:00+00','2020-06-01 10:00+00'), tstzrange('2021-01-01 00:00+00','2021-02-01 00:00+00'))::text
SELECT int4multirange(int4range(1,3), '[5,6]')::text
SELECT m @> '2020-03-01 00:00+00'::timestamptz FROM tm
RESET timezone
SELECT m::text FROM tm
