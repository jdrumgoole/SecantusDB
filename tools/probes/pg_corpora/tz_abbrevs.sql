# reference-version: 15
# Dynamic time zone abbreviations (MSK, VOLT, YAKT, ...) mean their zone's
# offset AT THE TIME given, and a zone abbreviation on a timetz is read.
set timezone = 'UTC'
select '2010-01-01 12:00 MSK'::timestamptz, '2012-01-01 12:00 MSK'::timestamptz, '2016-01-01 12:00 MSK'::timestamptz
select '1990-07-01 12:00 MSK'::timestamptz, '2010-07-01 12:00 MSD'::timestamptz
select '2014-12-01 00:00 MSK'::timestamptz, '2011-06-01 12:00 MSK'::timestamptz, '2011-01-01 12:00 MSK'::timestamptz
select '2012-01-01 12:00 VOLT'::timestamptz, '2019-01-01 12:00 VOLT'::timestamptz, '2021-01-01 12:00 VOLT'::timestamptz
select '2015-06-01 12:00 YAKT'::timestamptz, '2010-06-01 12:00 YAKST'::timestamptz, '2014-06-01 12:00 NOVT'::timestamptz
select '12:00 MSK'::timetz
select '12:00 EST'::timetz, '12:00:30.5 PDT'::timetz, '1:00 pm MSK'::timetz
