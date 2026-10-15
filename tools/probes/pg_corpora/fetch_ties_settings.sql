# reference-version: 15
# FETCH FIRST n ROWS WITH TIES (the rows after the n-th that tie with it under the ORDER BY; no bound on the Sort), SET x TO DEFAULT / = DEFAULT as the reset value (SET LOCAL ... TO DEFAULT undone with its block), pg_settings.enumvals from PostgreSQL 15.19, and a typed literal (int4'1') under standard_conforming_strings = off. Ends with every setting back at its default.
select k from fts_dt order by v fetch first 2 rows with ties
select k from fts_dt order by v offset 3 fetch first 2 rows with ties
select k from fts_dt order by v offset 13 fetch next 1 row with ties
select k from fts_dt order by v offset 20 fetch next 1 row with ties
select k from fts_dt fetch first 2 rows with ties
select k from fts_dt order by v fetch first 0 rows with ties
select k from fts_dt order by v fetch first rows with ties
select k from fts_dt order by v fetch first null rows with ties
select k from fts_dt order by v fetch first -1 rows with ties
select k from fts_dt order by v desc fetch first 1 row with ties
select k from fts_dt order by v desc, s fetch first 1 row with ties
select k, s from fts_dt order by s fetch first 1 row with ties
select k from fts_dt order by v, k fetch first 2 rows with ties
select k from fts_dt order by v + 0 fetch first 2 rows with ties
select count(*) from (select k from fts_dt order by v fetch first 1 row with ties) s
with c as (select k, v from fts_dt order by v desc fetch first 1 row with ties) select count(*) from c
select count(*) from (select k from fts_dt order by v fetch first 1 row with ties) s where k > 5
select k from fts_dt order by v fetch first 2 rows only
select k from fts_dt order by v limit 2
select k from fts_dt order by v fetch first 2 rows with ties for update
select k from fts_dt order by v fetch first 2 rows with ties for update skip locked
select distinct v from fts_dt order by v fetch first 1 row with ties
select v, count(*) from fts_dt group by v order by v fetch first 1 row with ties
select g from fts_dg group by g order by g desc fetch first 2 rows with ties
select k from fts_dt where v = 2 order by v fetch first 1 row with ties
select k from fts_dt where false order by v fetch first 1 row with ties
select k from fts_dt union all select k from fts_dt order by 1 fetch first 1 row with ties
select a.k from fts_dt a join fts_dg b on a.k = b.k order by b.g fetch first 1 row with ties
select k, row_number() over (order by k) from fts_dt order by v fetch first 1 row with ties
select k from fts_dt order by v fetch first 2 rows with ties offset 1
select k from fts_dt order by v fetch first $1 rows with ties
select a.k from fts_dt a join fts_dg b on a.k = b.k order by b.g desc fetch first 2 rows with ties
select k from fts_dt union select k + 100 from fts_dt where k < 3 order by 1 desc fetch first 1 row with ties
select g, count(*) from fts_dg group by g order by count(*), g fetch first 1 row with ties
set work_mem = '8MB'
set work_mem to default
show work_mem
set escape_string_warning = off
set escape_string_warning = default
show escape_string_warning
set backslash_quote = off
set backslash_quote to default
show backslash_quote
set standard_conforming_strings = off
set standard_conforming_strings to default
show standard_conforming_strings
set datestyle = 'German'
set datestyle to default
show datestyle
set search_path = foo
set search_path to default
show search_path
set statement_timeout = 5000
set statement_timeout = default
show statement_timeout
set intervalstyle = 'iso_8601'
set intervalstyle to default
show intervalstyle
set application_name = 'x'
set application_name to default
show application_name
set datestyle = 'German'
begin
set local datestyle to default
show datestyle
commit
show datestyle
begin
set datestyle to default
show datestyle
rollback
show datestyle
begin
set datestyle to default
commit
show datestyle
set datestyle = 'German'
begin
savepoint a
set datestyle to default
rollback to a
show datestyle
commit
set local datestyle to default
show datestyle
reset datestyle
set my.custom = 'x'
set my.custom to default
show my.custom
set transaction_isolation to default
set role to default
show role
select current_setting('work_mem')
select name, enumvals from pg_settings where vartype = 'enum' order by lower(name)
select enumvals[1], array_length(enumvals, 1) from pg_settings where name = 'ssl_max_protocol_version'
select pg_typeof(enumvals) from pg_settings where name = 'bytea_output'
select count(*) from pg_settings where enumvals is not null
select name from pg_settings where 'hex' = any(enumvals) order by 1
select enumvals from pg_settings where name = 'work_mem'
set standard_conforming_strings = off
select int4'1', date'2020-01-02', text'a\\b', varchar'x', pg_catalog.int4'2', "int4"'3', interval'1 day', timestamp'2020-01-01 10:00', bool't'
select pg_typeof(int4'1'), pg_typeof(numeric'1.5'), pg_typeof(point'(1,2)')
select n'a' || x'1f'::text, b'101'
reset standard_conforming_strings
select int4'1', date'2020-01-02'
select count(*) from pg_settings where vartype = 'enum' and enumvals is null
