# reference-version: 15
# CREATE CAST / DROP CAST: function, WITH INOUT and binary casts; explicit,
# assignment and implicit contexts; pg_cast; the 42P17 / 42710 / 2BP01 rules;
# and the composite-returning SQL functions a cast function usually is.
create type cst_pt as (a int, b text)
create type cst_mood as enum ('happy', 'sad')
create domain cst_dom as int
create function cst_f(int) returns cst_pt language sql as 'select $1, ''x''::text'
create function cst_g(text) returns int language sql as 'select length($1)'
create function cst_h(cst_pt) returns int language sql as 'select ($1).a * 10'
create cast (int as cst_pt) with function cst_f(int)
select 5::cst_pt
select (7::cst_pt).b
select cast(3 as cst_pt)
create cast (cst_pt as int) with function cst_h(cst_pt) as assignment
select (row(4, 'q')::cst_pt)::int
create table cst_t (n int, p cst_pt)
insert into cst_t values (1, row(2, 'y'))
select p::int from cst_t
select n from cst_t where p::int = 20
create cast (int as text) with inout
create cast (int as cst_mood) with inout
select 1::cst_mood
create cast (cst_mood as int) without function
create cast (int as cst_pt) with function cst_g(text)
create cast (text as cst_dom) with inout
create cast (int as cst_pt) with function cst_f(int)
create cast (int as nosuchtype) with inout
create cast (int as int) with inout
create cast (bigint as cst_pt) with function nosuch(bigint)
create cast (text as cst_mood) with inout as implicit
select castcontext, castmethod from pg_cast where castsource = 'int4'::regtype and casttarget = 'cst_pt'::regtype
select count(*) from pg_cast where casttarget = 'cst_mood'::regtype
select 'happy'::text::cst_mood
drop cast (int as cst_pt)
drop cast (int as cst_pt)
drop cast if exists (int as cst_pt)
select 5::cst_pt
drop cast (cst_pt as int)
drop cast (text as cst_mood)
drop cast if exists (int as text)
drop cast if exists (int as cst_mood)
drop cast if exists (text as cst_dom)
drop table cst_t
drop function cst_f(int)
drop function cst_g(text)
drop function cst_h(cst_pt)
drop domain cst_dom
drop type cst_pt
drop type cst_mood
create type cdp_pt as (a int, b text)
create type cdp_mood as enum ('happy', 'sad')
create domain cdp_dom as int
create function cdp_f(int) returns cdp_pt language sql as 'select $1, ''x''::text'
create cast (int as cdp_pt) with function cdp_f(int)
create cast (int as cdp_mood) with inout
create cast (text as cdp_dom) with inout
drop function cdp_f(int)
drop type cdp_mood
drop domain cdp_dom
drop type cdp_pt
drop type cdp_mood cascade
select count(*) from pg_cast where casttarget::regtype::text like 'cdp%'
drop domain cdp_dom cascade
drop function cdp_f(int) cascade
select count(*) from pg_cast where casttarget::regtype::text like 'cdp%'
select 5::cdp_pt
drop type cdp_pt
create type cx_pt as (a int, b text)
create function cx_h(cx_pt) returns int language sql as 'select ($1).a * 10'
create function cx_k(int) returns cx_pt language sql as 'select $1, ''k''::text'
create table cx_t (n int, p cx_pt)
insert into cx_t (n) values (row(3, 'q')::cx_pt)
create cast (cx_pt as int) with function cx_h(cx_pt) as assignment
insert into cx_t (n) values (row(3, 'q')::cx_pt)
insert into cx_t (n) select row(4, 'r')::cx_pt
update cx_t set n = row(5, 's')::cx_pt where n = 30
select n from cx_t order by n
select 1 + row(1, 'x')::cx_pt
create cast (int as cx_pt) with function cx_k(int) as implicit
insert into cx_t (p) values (7)
select p from cx_t where p is not null
select count(*) from cx_t where p = 7
drop cast (cx_pt as int)
drop cast (int as cx_pt)
drop table cx_t
drop function cx_h(cx_pt)
drop function cx_k(int)
drop type cx_pt
create type cstz as (a int, b text)
create function cstz_f(int) returns cstz language sql as 'select $1, ''x''::text'
select cstz_f(5)
select (cstz_f(5)).b
select (cstz_f(5)).*
select * from cstz_f(5)
create function cstz_g(int) returns cstz language sql as 'select row($1, ''y'')::cstz'
select cstz_g(1)
select (cstz_g(1)).a
create function cstz_h() returns cstz language sql as 'select 1, ''z''::text where false'
select cstz_h() is null
drop function cstz_f(int)
drop function cstz_g(int)
drop function cstz_h()
drop type cstz
