# reference-version: 15
# Assignment and date/time input errors: codes (22P02 / 22007 / 22008 / 42804),
# positions at the literal, and the datestyle hint for a month / day overflow.
create table as_t (i int, d date, ts timestamp, b bool, n numeric(6,2), v varchar(3))
insert into as_t values (1, '2020-01-01', '2020-01-01 00:00', true, 1.5, 'ab')
update as_t set i = 'x'
update as_t set i = true
update as_t set d = 'x'
update as_t set d = '2020-13-01'
update as_t set ts = '2020-01-01 25:00'
update as_t set b = 3
update as_t set n = 'abc'
update as_t set v = 'abcdef'
insert into as_t (i) values (true)
insert into as_t (d) values (5)
select length('abc'), ascii('a'), i, n, v, n * 2 as m, v::varchar(2) as w from as_t
drop table as_t
select '2020-13-01'::date
select '2020-02-30'::date
select '2020-01-01 25:00'::timestamp
select date '2020-13-01'
select '{{a,b},{c,"d e"}}'::varchar[];
select array[array['a','b'],array['c',null]]::bpchar[];
select '{{x}}'::name[];
select '{{"a b",NULL},{c,d}}'::varchar(3)[];
