# reference-version: 15
# ALTER ... RENAME for indexes, constraints, sequences, types (and their
# values and attributes), domains (and constraints), schemas, triggers, rules.
create table rn_t (id int primary key, v int unique, w int, constraint rn_ck check (w > 0))
create index rn_i on rn_t (w)
alter index rn_i rename to rn_i2
select indexname from pg_indexes where tablename = 'rn_t' order by 1
alter table rn_t rename constraint rn_ck to rn_ck2
alter table rn_t rename constraint rn_t_v_key to rn_vk
alter table rn_t rename constraint rn_t_pkey to rn_pk
select conname from pg_constraint where conrelid = 'rn_t'::regclass order by 1
select indexname from pg_indexes where tablename = 'rn_t' order by 1
insert into rn_t values (1, 1, 1), (2, 1, 1)
create sequence rn_s
alter sequence rn_s rename to rn_s2
select nextval('rn_s2')
create type rn_e as enum ('a', 'b')
alter type rn_e rename to rn_e2
alter type rn_e2 rename value 'a' to 'aa'
select enum_range(null::rn_e2)
create type rn_c as (x int)
alter type rn_c rename attribute x to y
select attname from pg_attribute where attrelid = (select typrelid from pg_type where typname = 'rn_c')
create domain rn_d as int
alter domain rn_d rename to rn_d2
create schema rn_sc
alter schema rn_sc rename to rn_sc2
create function rn_tf() returns trigger language plpgsql as $$ begin return new; end $$
create trigger rn_tr before insert on rn_t for each row execute function rn_tf()
alter trigger rn_tr on rn_t rename to rn_tr2
select tgname from pg_trigger where tgrelid = 'rn_t'::regclass
create rule rn_r as on update to rn_t do also select 1
alter rule rn_r on rn_t rename to rn_r2
select rulename from pg_rules where tablename = 'rn_t'
create statistics rn_st on v, w from rn_t
alter statistics rn_st rename to rn_st2
create view rn_v as select 1 as a
alter view rn_v rename column a to b
select * from rn_v
create type rn_mood as enum ('sad', 'ok')
create table rn_u (id int, m rn_mood)
insert into rn_u values (1, 'sad')
alter type rn_mood rename to rn_feel
select id, m, pg_typeof(m) from rn_u
insert into rn_u values (2, 'ok'::rn_feel)
select 'ok'::rn_mood
alter type rn_feel rename value 'sad' to 'blue'
select m from rn_u order by id
alter type rn_feel rename to rn_u
alter type rn_nope rename to rn_x
alter type if exists rn_nope rename to rn_x
create domain rn_pos as int constraint rn_pos_ck check (value > 0)
alter domain rn_pos rename constraint rn_pos_ck to rn_pos_ck2
select conname from pg_constraint where contypid = 'rn_pos'::regtype
alter domain rn_pos rename constraint rn_nope to rn_x
create schema rn_s1
create type rn_s1.rn_st as (a int)
alter schema rn_s1 rename to rn_s2
select (row(1)::rn_s2.rn_st).a
alter schema rn_nope rename to rn_x
alter index rn_nope rename to rn_x
alter index if exists rn_nope rename to rn_x
create table rn_w (a int)
create index rn_wi on rn_w (a)
alter index rn_wi rename to rn_w
alter trigger rn_nope on rn_w rename to rn_x
alter table rn_w rename constraint rn_nope to rn_x
drop table rn_w
drop table rn_u
drop type rn_feel
drop domain rn_pos
drop type rn_s2.rn_st
drop schema rn_s2
drop view rn_v
drop table rn_t
drop function rn_tf()
drop sequence rn_s2
drop type rn_e2
drop type rn_c
drop domain rn_d2
drop schema rn_sc2
