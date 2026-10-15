DROP VIEW IF EXISTS tvr2_v
DROP TABLE IF EXISTS tvr2_holder, tvr2_cols, tvr2_arr, tvr2_pt, tid, xid CASCADE
DROP TYPE IF EXISTS tvr2_mood, tvr2_comp CASCADE
DROP DOMAIN IF EXISTS tvr2_dint, tvr2_darr CASCADE
create table tvr2_arr (a int[], b tid[], c text[][], d point)
create type tvr2_mood as enum ('a', 'b')
create type tvr2_comp as (x int, y text)
create domain tvr2_dint as int
create domain tvr2_darr as int[]
create table tvr2_cols (a int[], b varchar(5)[], c int2vector, d oidvector, e tvr2_mood, f tvr2_comp, g tvr2_dint, h tvr2_darr, i tvr2_mood[], j name, k point, l bytea, m numeric(5,2)[], n timestamptz[])
create view tvr2_v as select a, e, i, c from tvr2_cols
create table tvr2_pt (id int, p point, j json, x xml, a point[], b box)
insert into tvr2_pt values (1, '(1,1)', '{}', '<a/>', '{"(1,1)"}', '(1,1),(0,0)')
create table tid (d date)
insert into tid values ('2020-01-01')
create table xid (a int, b text)
insert into xid values (1, 'x')
create table tvr2_holder (r public.tid, e tid)
insert into tvr2_holder values (row('2021-02-03'), '(0,2)')
