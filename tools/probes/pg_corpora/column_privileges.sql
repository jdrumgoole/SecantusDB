# reference-version: 15
# Column-level privileges (GRANT SELECT (a) ON t), attacl, relacl after a full
# revoke, and grants that follow a relation's drop and rename.
create role cp_r
create table cp_t (id int, v text, w int)
insert into cp_t values (1, 'a', 10), (2, 'b', 20)
grant select (v) on cp_t to cp_r
grant insert (id, v) on cp_t to cp_r
grant update (w) on cp_t to cp_r
select pg_typeof(relacl), replace(relacl::text, current_user, 'me') from pg_class where relname = 'cp_t'
select attname, replace(attacl::text, current_user, 'me') from pg_attribute where attrelid = 'cp_t'::regclass and attnum > 0 order by attnum
set role cp_r
select v from cp_t order by v
select id from cp_t
select * from cp_t
select count(*) from cp_t
select t.v from cp_t t order by 1
select t from cp_t t
select v from cp_t where id = 1
insert into cp_t (id, v) values (3, 'c')
insert into cp_t (id, v, w) values (4, 'd', 40)
insert into cp_t values (5, 'e', 50)
update cp_t set w = 11
update cp_t set v = 'x'
reset role
revoke select (v) on cp_t from cp_r
select attname, replace(attacl::text, current_user, 'me') from pg_attribute where attrelid = 'cp_t'::regclass and attnum > 0 order by attnum
grant select on cp_t to cp_r
revoke select on cp_t from cp_r
select pg_typeof(relacl), replace(relacl::text, current_user, 'me') from pg_class where relname = 'cp_t'
select attname, replace(attacl::text, current_user, 'me') from pg_attribute where attrelid = 'cp_t'::regclass and attnum > 0 order by attnum
grant select on cp_t to public
alter table cp_t rename to cp_t2
select replace(relacl::text, current_user, 'me') from pg_class where relname = 'cp_t2'
drop table cp_t2
create table cp_t2 (id int)
select replace(relacl::text, current_user, 'me') from pg_class where relname = 'cp_t2'
revoke all on cp_t2 from current_user
select replace(relacl::text, current_user, 'me') from pg_class where relname = 'cp_t2'
grant select, insert on cp_t2 to current_user
select replace(relacl::text, current_user, 'me') from pg_class where relname = 'cp_t2'
create table cp_u (a int, b int)
grant all (a) on cp_u to cp_r
select attname, replace(attacl::text, current_user, 'me') from pg_attribute where attrelid = 'cp_u'::regclass and attnum > 0 order by attnum
grant select (nope) on cp_u to cp_r
grant delete (a) on cp_u to cp_r
grant select (b) on cp_u to cp_r
alter table cp_u rename column a to a2
select attname, replace(attacl::text, current_user, 'me') from pg_attribute where attrelid = 'cp_u'::regclass and attnum > 0 and not attisdropped order by attnum
alter table cp_u drop column b
alter table cp_u add column b int
select attname, replace(attacl::text, current_user, 'me') from pg_attribute where attrelid = 'cp_u'::regclass and attnum > 0 and not attisdropped order by attnum
drop table cp_u
drop table cp_t2
drop role cp_r
