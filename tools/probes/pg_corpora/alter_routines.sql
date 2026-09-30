# reference-version: 15
# ALTER FUNCTION / PROCEDURE / ROUTINE: rename, attributes, SET SCHEMA,
# OWNER TO, SET / RESET configuration, and their errors.
create function ar_f(a int) returns int language sql as 'select a + 1'
alter function ar_f(int) rename to ar_g
select ar_g(1)
alter function ar_g(int) immutable
select provolatile from pg_proc where proname = 'ar_g'
alter function ar_g(int) strict cost 5
select proisstrict, procost from pg_proc where proname = 'ar_g'
alter function ar_g(int) cost 0
alter function ar_g(int) rows 10
alter function ar_g(int) security definer leakproof
select prosecdef, proleakproof from pg_proc where proname = 'ar_g'
alter function ar_g(int) parallel safe
select proparallel from pg_proc where proname = 'ar_g'
create function ar_h(a int) returns int language sql as 'select a'
create function ar_h(a text) returns text language sql as 'select a'
alter function ar_h rename to ar_hh
alter function ar_h(int) rename to ar_g
create schema ar_s
alter function ar_g(int) set schema ar_s
select ar_s.ar_g(2)
select n.nspname from pg_proc p join pg_namespace n on n.oid = p.pronamespace where proname = 'ar_g'
alter function ar_s.ar_g(int) set schema ar_nope
create role ar_r
alter function ar_s.ar_g(int) owner to ar_r
select pg_get_userbyid(proowner) from pg_proc where proname = 'ar_g'
alter function ar_s.ar_g(int) owner to ar_nobody
alter function ar_s.ar_g(int) set search_path = public
alter function ar_s.ar_g(int) set work_mem = '64kB'
select proconfig from pg_proc where proname = 'ar_g'
alter function ar_s.ar_g(int) reset work_mem
select proconfig from pg_proc where proname = 'ar_g'
alter function ar_s.ar_g(int) reset all
select proconfig from pg_proc where proname = 'ar_g'
alter function ar_nosuch(int) rename to x
alter function ar_nosuch rename to x
create procedure ar_p() language sql as 'select 1'
alter procedure ar_p() rename to ar_q
call ar_q()
alter routine ar_q() rename to ar_p
alter function ar_p() rename to ar_x
alter procedure ar_h(int) rename to ar_x
drop procedure ar_p()
drop function ar_s.ar_g(int)
drop function ar_h(int)
drop function ar_h(text)
drop schema ar_s
drop role ar_r
