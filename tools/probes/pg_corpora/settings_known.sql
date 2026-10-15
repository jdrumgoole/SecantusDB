# reference-version: 15
# A name that is not a parameter is 42704 for SET / SET LOCAL / SET ... TO DEFAULT / RESET / SHOW / set_config / current_setting, while a dotted (custom) name is a placeholder; the parameters pg_settings hides, and the machine-specific ones, are still parameters (55P02 where they cannot change). RESET transaction_isolation outside a block warns. Errors keep their positions under standard_conforming_strings = off, a missing $N is placed at it, and a FROM-less select obeys LIMIT / OFFSET (RP-036). Ends with every setting back at its default.
set nosuch_param = 1
set local nosuch_param = 1
set nosuch_param to default
reset nosuch_param
show nosuch_param
select set_config('nosuch_param', '1', false)
select current_setting('nosuch_param')
select current_setting('nosuch_param', true)
set myapp.x = 1
show myapp.x
select current_setting('myapp.x')
reset myapp.y
show myapp.y
show myapp.zz
set "NoSuch" = 1
set Work_Mem = '5MB'
show work_mem
reset work_mem
set session nosuch_param = 1
set transaction_isolation to default
reset transaction_isolation
begin
reset transaction_isolation
set transaction_isolation to default
commit
set port = 5
set log_timezone = 'UTC'
set data_directory = '/x'
set extra_float_digits = 3
set application_name = 'x'
set client_min_messages = warning
set lc_messages = 'C'
set search_path = public
set intervalstyle = 'postgres'
set datestyle = 'ISO'
set timezone = 'UTC'
set bytea_output = 'hex'
set standard_conforming_strings = on
set client_encoding = 'UTF8'
set default_transaction_isolation = 'read committed'
set statement_timeout = 0
set jit = off
set server_version = '1'
reset all
set seed = 0.5
set ssl_renegotiation_limit = 0
set default_with_oids = off
reset seed
reset server_version
reset port
select 1 fetch first $1 rows only
select $2
select 1, $3 + 1
select 1 limit $1
set standard_conforming_strings = off
select 1, 'x'::int4
select 'a\\b', 1/0
select 'x\'y', 'z'::int4
select N'q', 'r'::date
select 1, nosuchcol
select E'a', 'b'::int
select int4'x'
select 'a', int4'x', 'y'::int
select 'a', nosuchfn('b')
select 'é', 'x'::int4
select 'ab\'
select N'q', 'é'::int
select 'a' from nosuchtable
select $1, 'a'
reset standard_conforming_strings
select 1, 'x'::int4
select 1 limit 0
select 1 offset 1
select 1 limit $1
select 1 limit -1
select 1 order by 1 fetch first 1 row with ties
select 1 as a, 'x' as b, null as c, 2.5 as d limit 1
select pg_typeof('x'), 'x' limit 5
select 1 limit null
select 1 limit all
select now() < now() + interval '1 day' limit 1
select 1 union select 2 limit 1
select 1 offset 0
select 'a' || 'b' offset 0 rows fetch next 1 row only
select 1 where false limit 1
select generate_series(1, 5) limit 2
select count(*) limit 0
show is_superuser
show session_authorization
select pg_client_encoding(), pg_typeof(pg_client_encoding())
