# reference-version: 15
# table_rewrite, pg_event_trigger_ddl_commands() for every command kind,
# dropped objects' oids, and the functions outside their event (39P03).
create table etc_log (ev text, tag text, info text)
create function etc_rw() returns event_trigger language plpgsql as $$ begin insert into etc_log values (tg_event, tg_tag, pg_event_trigger_table_rewrite_oid()::regclass::text || ':' || pg_event_trigger_table_rewrite_reason()); end $$
create event trigger etc_rw_t on table_rewrite execute function etc_rw()
create table etc_t (a int, b text)
insert into etc_t values (1, 'x')
alter table etc_t alter column a type bigint
alter table etc_t add column c int default 5
alter table etc_t add column d int
select * from etc_log order by 1, 2
drop event trigger etc_rw_t
create function etc_end() returns event_trigger language plpgsql as $$ declare r record; begin for r in select * from pg_event_trigger_ddl_commands() loop insert into etc_log values ('end', r.command_tag, r.object_type || ' ' || coalesce(r.object_identity, '')); end loop; end $$
create event trigger etc_end_t on ddl_command_end execute function etc_end()
create schema etc_s
comment on table etc_t is 'c'
alter table etc_t rename column b to bb
create view etc_v as select 1 as x
alter view etc_v rename to etc_v2
grant select on etc_t to public
create function etc_f() returns int language sql as 'select 1'
alter function etc_f() rename to etc_f2
select ev, tag, info from etc_log where ev = 'end' order by 2, 3
drop event trigger etc_end_t
create function etc_drop() returns event_trigger language plpgsql as $$ declare r record; begin for r in select * from pg_event_trigger_dropped_objects() loop insert into etc_log values ('drop', r.object_type, (r.objid <> 0)::text); end loop; end $$
create event trigger etc_drop_t on sql_drop execute function etc_drop()
drop function etc_f2()
drop view etc_v2
select ev, tag, info from etc_log where ev = 'drop' order by 2
select count(*) from (select command from pg_event_trigger_ddl_commands()) s
drop event trigger etc_drop_t
drop schema etc_s
drop table etc_t
drop table etc_log
drop function etc_rw()
drop function etc_end()
drop function etc_drop()
create table etn_log (tag text)
create function etn_f() returns event_trigger language plpgsql as $$ begin insert into etn_log values (tg_tag); end $$
create event trigger etn_t on ddl_command_end execute function etn_f()
do $$ begin create table etn_x (a int); end $$
create function etn_mk() returns void language plpgsql as $$ begin execute 'create table etn_y (a int)'; end $$
select etn_mk()
select tag from etn_log order by 1
drop event trigger etn_t
drop table etn_x
drop table etn_y
drop function etn_mk()
drop function etn_f()
drop table etn_log
create table etw_log (tag text, otype text, ident text)
create function etw_f() returns event_trigger language plpgsql as $$ declare r record; begin for r in select * from pg_event_trigger_ddl_commands() loop insert into etw_log values (r.command_tag, r.object_type, r.object_identity); end loop; end $$
create event trigger etw_t on ddl_command_end execute function etw_f()
create table etw_a (id int primary key, v text)
create function etw_trg() returns trigger language plpgsql as $$ begin return new; end $$
create trigger etw_tr before insert on etw_a for each row execute function etw_trg()
create rule etw_r as on update to etw_a do also select 1
create sequence etw_seq
alter sequence etw_seq increment by 2
create type etw_e as enum ('a')
alter type etw_e add value 'b'
create domain etw_d as int check (value > 0)
alter domain etw_d set default 1
alter table etw_a enable row level security
create policy etw_p on etw_a using (true)
create index etw_i on etw_a (v)
alter index etw_i rename to etw_i2
create statistics etw_st on id, v from etw_a
create publication etw_pub for table etw_a
alter table etw_a add column w int
select tag, otype, ident from etw_log order by 1, 2, 3
drop event trigger etw_t
drop publication etw_pub
drop table etw_a
drop function etw_trg()
drop sequence etw_seq
drop type etw_e
drop domain etw_d
drop function etw_f()
drop table etw_log
