# reference-version: 15
# Batch 11 catalog fixes: pg_table_size counts a TOAST index, a view's
# computed columns keep their cast's type modifier, pg_class.relacl renders
# GRANTs (owner name normalised: the two servers' session users differ), and
# relhasrules for a table with a rule.
create table sz_a (id int, t text)
create table sz_b (id int, d date, f float8)
create table sz_c (v varchar(10), n numeric(5,2))
create table sz_d (v varchar)
create table sz_e (a int[])
select relname, pg_table_size(oid), pg_relation_size(oid), pg_total_relation_size(oid) from pg_class where relname like 'sz_%' order by 1
insert into sz_a values (1, 'x')
select pg_table_size('sz_a') - pg_relation_size('sz_a')
create view sz_v as select id::numeric(5,2) as n, t::varchar(3) as v, 'a'::char(4) as c, id as i from sz_a
select attname, atttypmod, format_type(atttypid, atttypmod) from pg_attribute where attrelid = 'sz_v'::regclass and attnum > 0 order by attnum
select * from sz_v
create table acl_t (id int)
select relacl is null from pg_class where relname = 'acl_t'
create role acl_r
grant select, insert on acl_t to acl_r
select array_length(relacl, 1), replace(relacl[1]::text, current_user, 'U'), replace(relacl[2]::text, current_user, 'U') from pg_class where relname = 'acl_t'
grant update on acl_t to acl_r with grant option
select replace(relacl[2]::text, current_user, 'U') from pg_class where relname = 'acl_t'
grant select on acl_t to public
select replace(relacl[3]::text, current_user, 'U') from pg_class where relname = 'acl_t'
create rule acl_rl as on delete to acl_t do instead nothing
select relname, relhasrules from pg_class where relname in ('acl_t', 'sz_a') order by 1
revoke all on acl_t from acl_r
revoke all on acl_t from public
drop view sz_v
drop table sz_a, sz_b, sz_c, sz_d, sz_e, acl_t
drop role acl_r
