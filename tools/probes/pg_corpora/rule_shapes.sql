# reference-version: 15
# Rule actions of every shape the rewriter takes: several VALUES rows, DEFAULT
# in VALUES (the action's and the original's), a set operation (refused with
# 42P10), and an ON SELECT rule turning an empty table into a view.
create table rs_t (id int, v text default 'dv')
create table rs_log (id int, v text default 'logd')
create rule rs_mv as on insert to rs_t do also insert into rs_log values (new.id, 'a'), (new.id, 'b')
insert into rs_t values (1, 'x'), (2, default)
select * from rs_t order by id
select * from rs_log order by id, v
create rule rs_so as on update to rs_t do also insert into rs_log select old.id, 'u1' union all select new.id, 'u2'
create rule rs_df as on delete to rs_t do also insert into rs_log values (old.id, default)
delete from rs_t where id = 1
select * from rs_log order by id, v
create rule rs_cond as on update to rs_t where new.id > 1 do also insert into rs_log values (new.id, 'c1'), (new.id, 'c2')
update rs_t set v = 'y'
select * from rs_log order by id, v
create table rs_v (id int, v text)
create rule "_RETURN" as on select to rs_v do instead select * from rs_t
select relkind from pg_class where relname = 'rs_v'
select * from rs_v
create table rs_w (id int, v text)
insert into rs_w values (1, 'a')
create rule "_RETURN" as on select to rs_w do instead select * from rs_t
delete from rs_w
create rule "_RETURN" as on select to rs_w do instead select id from rs_t
create rule "_RETURN" as on select to rs_w do instead select id, v, v as w from rs_t
create rule "_RETURN" as on select to rs_w do instead select v, id from rs_t
create rule "_RETURN" as on select to rs_w do instead select id, id as v from rs_t
create rule "_RETURN" as on select to rs_w where true do instead select * from rs_t
create rule "_RETURN" as on select to rs_w do also select * from rs_t
create rule "other" as on select to rs_w do instead select * from rs_t
drop table rs_w
drop view rs_v
drop table rs_t
drop table rs_log
