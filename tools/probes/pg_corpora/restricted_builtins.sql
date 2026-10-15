# reference-version: 15
# The built-ins PostgreSQL ships with EXECUTE revoked from PUBLIC (pg_reload_conf, pg_stat_reset, pg_ls_waldir, ...): 42501 for a role without a GRANT or the predefined role, under SET ROLE, in any statement shape; a GRANT lets it call (RP-013).
set role rl_r
select pg_reload_conf()
reset role
grant execute on function pg_reload_conf() to rl_r
set role rl_r
select pg_reload_conf()
reset role
revoke execute on function pg_reload_conf() from rl_r
set role rl_r
select pg_stat_reset()
select pg_ls_waldir()
select abs(-1)
select pg_reload_conf() from (select 1) s
reset role
select pg_reload_conf()
grant execute on function pg_reload_conf() to rl_r
set role rl_r
select pg_reload_conf() from (select 1) s
reset role
revoke execute on function pg_reload_conf() from rl_r
select has_function_privilege('rl_r', 'pg_reload_conf()', 'execute')
select has_function_privilege('rl_r', 'abs(int)', 'execute')
set role rl_r
select 1, pg_reload_conf()
select pg_reload_conf() where true
select x from (select pg_reload_conf() x) s
select pg_reload_conf() from (select 1) s
select s.a, pg_reload_conf() from (values (1)) s(a)
with c as (select 1) select pg_reload_conf() from c
reset role
