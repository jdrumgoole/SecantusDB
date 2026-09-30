# to_date / to_timestamp over short, malformed and ISO-week input.
select to_date('2020-13-01', 'YYYY-MM-DD')::text
select to_date('2020-02-30', 'YYYY-MM-DD')::text
select to_date('2020-01-32', 'YYYY-MM-DD')::text
select to_date('abcd-01-01', 'YYYY-MM-DD')::text
select to_date('2020-ab-01', 'YYYY-MM-DD')::text
select to_date('2020', 'YYYY-MM-DD')::text
select to_date('', 'YYYY')::text
select to_date('2020-01-01 junk', 'YYYY-MM-DD')::text
select to_timestamp('2020-01-01 25:00', 'YYYY-MM-DD HH24:MI')
select to_timestamp('2020-01-01 12:60', 'YYYY-MM-DD HH24:MI')
select to_timestamp('2020-01-01 13:00 PM', 'YYYY-MM-DD HH:MI AM')
select to_timestamp('2020-01-01 00:00', 'YYYY-MM-DD HH12:MI')
select to_date('Foo 1 2020', 'Mon DD YYYY')::text
select to_date('1 2020', 'DDD YYYY')::text
select to_date('367 2020', 'DDD YYYY')::text
select to_date('0 2020', 'DDD YYYY')::text
select to_date('2020-01-01', 'YYYY-MM-DD-')::text
select to_date('20200101', 'YYYYMMDD')::text
select to_date('2020 366', 'YYYY DDD')::text
select to_date('2019 366', 'YYYY DDD')::text
select to_timestamp('2020-01-01 10:00:61', 'YYYY-MM-DD HH24:MI:SS')
select to_date('2020-W60-1', 'IYYY-"W"IW-ID')::text
select to_date('2020-01-01', 'YYYY-MM-DD Q')::text
select to_date('99999-01-01', 'YYYY-MM-DD')::text
select to_date('-2020-01-01', 'YYYY-MM-DD')::text
