# reference-version: 15
# Collations: ICU ones (-x-icu names and CREATE COLLATION provider = icu),
# nondeterministic ones in comparisons, IN, DISTINCT, GROUP BY and UNIQUE,
# column and index COLLATE, explicit-collation conflicts, and the catalog.
select x from (values ('b'), ('A'), ('a'), ('B'), ('é'), ('e')) v(x) order by x collate "C"
select x from (values ('b'), ('A'), ('a'), ('B'), ('é'), ('e')) v(x) order by x collate "und-x-icu"
select x from (values ('b'), ('A'), ('a'), ('B'), ('é'), ('e')) v(x) order by x collate "en-x-icu"
select 'a' < 'B' collate "C", 'a' < 'B' collate "und-x-icu"
select x from (values ('item10'), ('item9'), ('Item1')) v(x) order by x collate "und-x-icu"
create collation clx_ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false)
select 'ABC' = 'abc' collate clx_ci, 'ABC' = 'abc'
create table clx_t (s text collate clx_ci)
insert into clx_t values ('Apple'), ('apple'), ('BANANA')
select count(*) from clx_t where s = 'APPLE'
select distinct s from clx_t order by s
create collation clx_num (provider = icu, locale = 'und-u-kn-true')
select x from (values ('item10'), ('item9'), ('item1')) v(x) order by x collate clx_num
select collname, collprovider, collisdeterministic from pg_collation where collname like 'clx_%' order by 1
drop table clx_t
drop collation clx_ci
drop collation clx_num
create collation clx_ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false)
create collation clx_de (provider = icu, locale = 'de-u-co-phonebk')
create table clx_t (id int, s text collate clx_ci, d text)
insert into clx_t values (1, 'Apple', 'a'), (2, 'apple', 'b'), (3, 'BANANA', 'c'), (4, 'Äpfel', 'd')
select count(*) from clx_t where s like 'app%'
select 'a' = 'A' collate clx_ci collate "C"
select s collate "C" = d collate clx_ci from clx_t
select s = d from clx_t limit 1
select count(*) from clx_t where s in ('APPLE', 'banana')
select min(s), max(s) from clx_t
select s, count(*) from clx_t group by s order by 1
create unique index clx_u on clx_t (s)
create index clx_i on clx_t (d collate "C")
create index clx_i2 on clx_t (d collate clx_de)
select x from (values ('Müller'), ('Mueller'), ('Muller'), ('Mahler')) v(x) order by x collate clx_de
select x from (values ('b'), ('A'), ('a')) v(x) order by x collate "de-x-icu" desc
select a.attname, a.attcollation::regcollation::text from pg_attribute a where attrelid = 'clx_t'::regclass and attnum > 0 order by attnum
select column_name, collation_name from information_schema.columns where table_name = 'clx_t' order by ordinal_position
select pg_collation_for(s) from clx_t limit 1
select collation for ('x' collate clx_de)
drop table clx_t
drop collation clx_ci
drop collation clx_de
create collation clx_v (provider = icu, locale = 'de')
create collation clx_v (provider = icu, locale = 'de')
create collation if not exists clx_v (provider = icu, locale = 'de')
create collation clx_b (provider = icu)
create collation clx_c (locale = 'de', deterministic = false)
create collation clx_e from "und-x-icu"
create collation clx_g (provider = nope, locale = 'de')
create collation clx_h (provider = icu, locale = 'de', color = 'red')
select collname, collprovider, collisdeterministic, colliculocale from pg_collation where collname like 'clx%' order by 1
create table clx_n (n int collate "C")
select 'x' collate "no_such_collation"
select x from (values ('a'), ('b')) v(x) order by x collate "no_such"
drop collation clx_nope
drop collation if exists clx_nope
drop collation clx_v
drop collation clx_e
