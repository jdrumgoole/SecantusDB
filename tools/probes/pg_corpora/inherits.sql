# Table inheritance: a parent read or written without ONLY reaches its descendants.
create table ih_p (id int, name text, check (id > 0))
create table ih_c (extra text) inherits (ih_p)
create table ih_g (deep int) inherits (ih_c)
insert into ih_p values (1, 'p')
insert into ih_c values (2, 'c', 'x')
insert into ih_g values (3, 'g', 'y', 9)
select * from ih_p order by id
select * from only ih_p order by id
select * from ih_c order by id
select id from only ih_c order by id
select tableoid::regclass::text, id from ih_p order by id
select ih_p.tableoid::regclass::text, count(*) from ih_p group by 1 order by 1
select tableoid::regclass::text from only ih_p
select count(*) from ih_p where name <> 'p'
update ih_p set name = upper(name) where id >= 2
select * from ih_p order by id
delete from ih_p where id = 3
select * from ih_g
insert into ih_c values (-1, 'bad', 'z')
alter table ih_p add column added int default 7
select * from ih_c order by id
select column_name from information_schema.columns where table_name = 'ih_c' order by ordinal_position
select inhrelid::regclass::text, inhparent::regclass::text, inhseqno from pg_inherits where inhparent::regclass::text like 'ih%' order by 1
drop table ih_p
drop table ih_c cascade
select * from ih_p
create table ih_m (id text) inherits (ih_p)
create table ih_n (id int, other int) inherits (ih_p)
select column_name from information_schema.columns where table_name = 'ih_n' order by ordinal_position
truncate ih_p
select count(*) from ih_n
drop table ih_p cascade
select tableoid = 'ih_p'::regclass from ih_p where id = 1
