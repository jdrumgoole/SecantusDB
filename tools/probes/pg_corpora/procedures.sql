# reference-version: 15
# CREATE / CALL / DROP PROCEDURE, OUT and INOUT parameters, and functions
# with OUT parameters and no RETURNS.
create procedure pr_1(a int, inout b int, out c text) language plpgsql as $$ begin b := a + b; c := 'x'; end $$
create function pr_f() returns int language sql as 'select 1'
call pr_1(1, 2, null)
call pr_1(1, 2)
call pr_f()
select pr_1(1, 2, null)
select pr_1(1, 2)
drop procedure pr_f()
drop function pr_1(int, int, text)
drop function pr_1(int, int)
drop procedure pr_nosuch()
call pr_nosuch(1)
select proname, prokind, prorettype::regtype, pronargs, proargmodes, proallargtypes::text, proargnames from pg_proc where proname = 'pr_1'
create table pr_t (a int)
create procedure pr_2(n int) language sql as $$ insert into pr_t values (n); insert into pr_t values (n + 1) $$
call pr_2(10)
select * from pr_t order by a
create procedure pr_3(inout x int) language sql as $$ select x * 3 $$
call pr_3(4)
create procedure pr_4() language plpgsql as $$ begin insert into pr_t values (99); end $$
call pr_4()
select count(*) from pr_t
create procedure pr_1(a int, inout b int, out c text) language plpgsql as $$ begin end $$
create function pr_o(a int, out b int, inout c text) language sql as 'select a, c'
select * from pr_o(1, 'x')
select pr_o(1, 'x')
create function pr_p(a int, out b int) language plpgsql as $$ begin b := a * 10; end $$
select pr_p(4), * from pr_p(5)
create function pr_bad(a int) language sql as 'select 1'
create function pr_bad2(out b int, out c int) returns int language sql as 'select 1, 2'
drop routine pr_f()
drop procedure pr_1(int, int)
drop procedure pr_2
drop procedure pr_3
drop procedure pr_4()
drop function pr_o(int, text)
drop function pr_p(int)
drop table pr_t
