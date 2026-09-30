# reference-version: 15
# Rules (CREATE RULE on INSERT / UPDATE / DELETE: ALSO, INSTEAD, INSTEAD
# NOTHING, a WHERE), DROP RULE, ALTER TABLE ENABLE / DISABLE RULE and
# TRIGGER, pg_rewrite / pg_trigger.tgenabled, and pg_rules.definition as
# ruleutils prints it.
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
select rulename, definition from pg_rules where tablename = 'rl_t' order by 1
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
create table rd_t (id int primary key, v text, n numeric)
create table rd_log (op text, id int, v text)
create rule rd_1 as on insert to rd_t do also insert into rd_log values ('i', new.id, new.v)
create rule rd_2 as on update to rd_t where old.v <> new.v do also insert into rd_log (id, op) values (new.id, 'u')
create rule rd_3 as on delete to rd_t do instead (delete from rd_log where rd_log.id = old.id; insert into rd_log values ('d', old.id, null))
create rule rd_4 as on update to rd_t do also update rd_log set v = new.v, op = 'x' where id = old.id
create rule rd_5 as on insert to rd_t where new.n > 1 do instead nothing
select rulename, definition from pg_rules where tablename = 'rd_t' order by 1
drop table rd_t, rd_log
create table rw_t (id int, v int)
create table rw_log (n bigint, note text)
insert into rw_t values (1, 10), (2, 20), (3, 30)
create rule rw_u as on update to rw_t do also insert into rw_log values (1, 'upd')
update rw_t set v = v + 1
select count(*) from rw_log
create rule rw_d as on delete to rw_t do also insert into rw_log select count(*), 'del' from rw_t
delete from rw_t where id = 1
select n, note from rw_log where note = 'del' order by 1
create table rw_c (x int)
create rule rw_i as on insert to rw_c do also insert into rw_log select count(*), 'ins' from rw_c
insert into rw_c values (1), (2)
select n, note from rw_log where note = 'ins' order by 1
create table rw_s (x int)
create rule rw_si as on insert to rw_s do instead insert into rw_log values (new.x, 'redir')
insert into rw_s select generate_series(1, 3)
select n from rw_log where note = 'redir' order by 1
drop table rw_t, rw_log, rw_c, rw_s
create table rx_t (id serial primary key, v int default 7, w text)
create table rx_log (op text, id int, v int)
create rule rx_ci as on insert to rx_t where new.v > 100 do instead insert into rx_log values ('big', new.id, new.v)
insert into rx_t (v, w) values (5, 'a'), (500, 'b'), (null, 'c')
select id, v, w from rx_t order by id
select op, id, v from rx_log order by op, id
insert into rx_t default values
insert into rx_t (w) select 'x' || g from generate_series(1, 2) g
select id, v, w from rx_t order by id
create rule rx_cu as on update to rx_t where old.v < 6 do instead update rx_log set v = new.v where rx_log.id = old.id
update rx_t set v = v + 1
select id, v from rx_t order by id
create table rx_o (id int, n int)
insert into rx_o values (1, 10), (2, 20)
create rule rx_ar as on update to rx_o do also insert into rx_log values ('also', old.id, new.n)
update rx_o set n = n * 2 where id = 1 returning id, n
update rx_o o set n = s.x from (values (2, 99)) s(i, x) where o.id = s.i
select op, id, v from rx_log where op = 'also' order by id
create view rx_v as select id, n from rx_o
create rule rx_vi as on insert to rx_v do instead insert into rx_o values (new.id, new.n)
create rule rx_vd as on delete to rx_v do instead delete from rx_o where rx_o.id = old.id
insert into rx_v values (3, 30)
delete from rx_v where id = 1
select * from rx_o order by id
create table rx_r (x int)
create rule rx_rr as on insert to rx_r do also insert into rx_r values (new.x + 1)
insert into rx_r values (1)
create table rx_d (id int)
create rule rx_dd as on delete to rx_d do instead nothing
insert into rx_d values (1), (2)
delete from rx_d
insert into rx_t (v) values (1000) returning id
drop view rx_v
drop table rx_t, rx_log, rx_o, rx_r, rx_d
