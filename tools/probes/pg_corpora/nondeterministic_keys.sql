# reference-version: 15
# Keys under a NONDETERMINISTIC collation: a PRIMARY KEY or UNIQUE constraint
# refuses what the collation calls equal, and count(DISTINCT) counts keys.
create collation ndk_ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false)
create table ndk_t (k text collate ndk_ci primary key, v int)
insert into ndk_t values ('Apple', 1)
insert into ndk_t values ('apple', 2)
insert into ndk_t values ('Pear', 3), ('PEAR', 4)
update ndk_t set k = 'APPLE' where v = 1
update ndk_t set k = 'pear' where v = 1
select k, v from ndk_t order by v
create table ndk_u (k text collate ndk_ci unique, n int)
insert into ndk_u values ('Apple', 1), (null, 2), (null, 3)
insert into ndk_u values ('APPLE', 4)
select count(*) from ndk_u
create table ndk_g (k text collate ndk_ci, v int)
insert into ndk_g values ('Apple', 1), ('apple', 2), ('Pear', 3)
select count(distinct k) from ndk_g
select count(*) from (select distinct k from ndk_g) s
select count(*), min(v) from ndk_g group by k order by 2
select count(distinct k) filter (where v > 1) from ndk_g
drop table ndk_g
drop table ndk_u
drop table ndk_t
drop collation ndk_ci
