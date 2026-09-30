# reference-version: 15
# Rules (CREATE RULE on INSERT / UPDATE / DELETE: ALSO, INSTEAD, INSTEAD
# NOTHING, a WHERE), DROP RULE, ALTER TABLE ENABLE / DISABLE RULE and
# TRIGGER, and pg_rewrite / pg_trigger.tgenabled.
create table rl_t (id int primary key, v text)
create table rl_log (op text, id int, v text)
create rule rl_ins as on insert to rl_t do also insert into rl_log values ('i', new.id, new.v)
insert into rl_t values (1, 'a'), (2, 'b')
select * from rl_log order by id
create rule rl_upd as on update to rl_t where old.v <> new.v do also insert into rl_log values ('u', new.id, old.v || '>' || new.v)
update rl_t set v = 'z' where id = 1
update rl_t set v = v where id = 2
select * from rl_log order by op, id
create rule rl_del as on delete to rl_t do instead nothing
delete from rl_t where id = 1
select count(*) from rl_t
create table rl_ro (id int)
create rule rl_ro_ins as on insert to rl_ro do instead insert into rl_log values ('redirect', new.id, null)
insert into rl_ro values (7)
select count(*) from rl_ro
select * from rl_log where op = 'redirect'
select rulename, ev_type, is_instead from pg_rewrite r join pg_class c on c.oid = r.ev_class where c.relname like 'rl_%' order by 1
create rule rl_ins as on insert to rl_t do also nothing
create or replace rule rl_ins as on insert to rl_t do also nothing
insert into rl_t values (3, 'c')
select count(*) from rl_log where op = 'i'
drop rule rl_ins on rl_t
drop rule rl_nope on rl_t
drop rule if exists rl_nope on rl_t
alter table rl_t disable rule rl_del
delete from rl_t where id = 3
select count(*) from rl_t
create rule rl_sel as on select to rl_ro do instead select 1 as id
create function rl_tf() returns trigger language plpgsql as $$ begin new.v := upper(new.v); return new; end $$
create trigger rl_trg before insert on rl_t for each row execute function rl_tf()
insert into rl_t values (10, 'x')
alter table rl_t disable trigger rl_trg
insert into rl_t values (11, 'y')
alter table rl_t enable trigger all
insert into rl_t values (12, 'w')
select id, v from rl_t where id >= 10 order by id
select tgname, tgenabled from pg_trigger where tgname = 'rl_trg'
alter table rl_t disable trigger rl_nope
select rulename, ev_enabled from pg_rewrite where rulename = 'rl_del'
select rulename, definition from pg_rules where rulename = 'rl_del'
drop table rl_t, rl_log, rl_ro
drop function rl_tf()
