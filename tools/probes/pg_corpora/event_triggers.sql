# reference-version: 15
# Event triggers: CREATE / ALTER / DROP EVENT TRIGGER and their validation,
# ddl_command_start / ddl_command_end / sql_drop with TG_EVENT and TG_TAG,
# pg_event_trigger_dropped_objects() and pg_event_trigger_ddl_commands()
# (TOAST rows filtered: this server has no TOAST tables), pg_event_trigger;
# DROP FUNCTION of several functions (all or nothing); array fields of a
# PL/pgSQL record.
create table ev_log (ev text, tag text)
create function ev_f() returns event_trigger language plpgsql as $$ begin insert into ev_log values (tg_event, tg_tag); end $$
create event trigger ev_start on ddl_command_start execute function ev_f()
create event trigger ev_end on ddl_command_end when tag in ('CREATE TABLE') execute function ev_f()
create table ev_a (id int)
create index on ev_a (id)
select * from ev_log
select evtname, evtevent, evtenabled, evttags from pg_event_trigger order by 1
alter event trigger ev_start disable
create table ev_b (id int)
select count(*) from ev_log
create event trigger ev_start on ddl_command_start execute function ev_f()
create event trigger ev_bad on nonsense execute function ev_f()
create function ev_g() returns event_trigger language plpgsql as $$ declare r record; begin for r in select * from pg_event_trigger_dropped_objects() loop insert into ev_log values ('drop', r.object_type || ':' || r.object_identity); end loop; end $$
create event trigger ev_drop on sql_drop execute function ev_g()
drop table ev_b
select * from ev_log where ev = 'drop' order by tag
drop event trigger ev_start
drop event trigger ev_end
drop event trigger ev_drop
drop event trigger ev_nope
drop event trigger if exists ev_nope
drop table ev_a, ev_log
drop function ev_f(), ev_g()
create table evp_log (n serial, a text, b text, c text, d bool, e bool, f text, g text[], h text[])
create function evp_g() returns event_trigger language plpgsql as $$ declare r record; begin for r in select * from pg_event_trigger_dropped_objects() loop insert into evp_log (a, b, c, d, e, f, g, h) values (r.object_type, r.schema_name, r.object_identity, r.original, r.normal, r.object_name, r.address_names, r.address_args); end loop; end $$
create function evp_c() returns event_trigger language plpgsql as $$ declare r record; begin for r in select * from pg_event_trigger_ddl_commands() loop insert into evp_log (a, b, c, d, f) values ('CMD:'||r.command_tag, r.object_type, r.object_identity, r.in_extension, r.schema_name); end loop; end $$
create event trigger evp_d on sql_drop execute function evp_g()
create event trigger evp_e on ddl_command_end execute function evp_c()
create table evp_t (id int primary key, v text unique, s serial)
create index evp_i on evp_t (v)
create view evp_v as select * from evp_t
create function evp_f(a int, b text) returns int language sql as 'select 1'
create sequence evp_s
create type evp_ty as (a int)
alter table evp_t add column w int
select a, b, c, d, f from evp_log order by n
delete from evp_log
drop view evp_v
drop index evp_i
drop function evp_f(int, text)
drop sequence evp_s
drop type evp_ty
select a, b, c, d, e, f, g, h from evp_log order by n
delete from evp_log
drop table evp_t
select a, b, c, d, e, f, g, h from evp_log where coalesce(b, '') <> 'pg_toast' order by a, c
create function evp_bad() returns int language sql as 'select 1'
create event trigger evp_x on ddl_command_start execute function evp_bad()
create event trigger evp_x on ddl_command_start execute function evp_nope()
create event trigger evp_x on ddl_command_start when tag in ('create table', 'BOGUS') execute function evp_c()
create event trigger evp_x on ddl_command_start when tag in ('CREATE DATABASE') execute function evp_c()
create event trigger evp_x on ddl_command_start when flavour in ('CREATE TABLE') execute function evp_c()
alter event trigger evp_nope enable
alter event trigger evp_e enable always
select evtname, evtenabled from pg_event_trigger where evtname like 'evp_%' order by 1
drop event trigger evp_d
drop event trigger evp_e
drop table evp_log
drop function evp_g(), evp_c(), evp_bad()
create function df_a() returns int language sql as 'select 1'
create function df_b(x int) returns int language sql as 'select x'
drop function df_a(), df_nope()
select proname from pg_proc where proname like 'df_%' order by 1
drop function if exists df_a(), df_nope(), df_b(int)
select proname from pg_proc where proname like 'df_%' order by 1
create table rc_t (g text[])
create table rc_s (a text[])
insert into rc_s values (array['x','y'])
do $$ declare r record; begin for r in select * from rc_s loop insert into rc_t values (r.a); end loop; end $$
do $$ declare r record; begin for r in select * from pg_event_trigger loop insert into rc_t values (r.evttags); end loop; end $$
do $$ declare r record; begin for r in select a from rc_s loop insert into rc_t values (r.a); end loop; end $$
select * from rc_t
create trigger rc_trg before insert on rc_t for each row execute function rc_nope()
drop table rc_t, rc_s
