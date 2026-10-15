# reference-version: 15
# pg_timezone_names / pg_timezone_abbrevs, as views and as functions in FROM (RP-015). Morocco's two zones are left out: PostgreSQL's compiled tz data and chrono-tz's disagree on them (timezones.rs).
select count(*) from pg_timezone_abbrevs
select * from pg_timezone_abbrevs order by abbrev
select name, abbrev, utc_offset, is_dst from pg_timezone_names where name not in ('Africa/Casablanca', 'Africa/El_Aaiun') order by name
select * from pg_timezone_names where name = 'Europe/Dublin'
select pg_typeof(utc_offset)::text, pg_typeof(is_dst)::text from pg_timezone_names limit 1
select name from pg_timezone_names where name ilike '%london%'
select count(*) from pg_timezone_names()
select count(*) from pg_timezone_abbrevs()
select t.abbrev from pg_timezone_abbrevs() t where t.abbrev like 'U%' order by 1
select * from pg_timezone_abbrevs() where abbrev = 'IST'
select a.abbrev from pg_timezone_abbrevs() as a(abbrev, o, d) where a.abbrev = 'EST'
select name from pg_catalog.pg_timezone_names where name = 'UTC'
select n.name from pg_timezone_names n join pg_timezone_abbrevs a on a.abbrev = n.abbrev where n.name = 'Europe/Dublin'
