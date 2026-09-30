# reference-version: 15
# pg_get_viewdef / pg_views.definition as ruleutils prints the analysed
# query: qualified columns, typed literals, implicit casts, parenthesised
# operators, and appendContextKeyword layout (PostgreSQL 15 qualifies even a
# single-relation view), and the pretty form (`pg_get_viewdef(v, true)`).
create table vd_t (id int primary key, v varchar(10), t text, n numeric, d date, b bool)
create table vd_u (id int, t_id int, w text)
create view vd_1 as select id, t from vd_t
create view vd_2 as select id, t from vd_t where id > 1 and t = 'x'
create view vd_3 as select v, count(*) as c from vd_t group by v having count(*) > 1 order by v
create view vd_4 as select a.id, b.w from vd_t a join vd_u b on b.t_id = a.id
create view vd_5 as select id + 1 as p, upper(t) as u, coalesce(t, 'z') as c, n * 2.5 as m from vd_t
create view vd_6 as select * from vd_t where v = 'abc' or n > 1.5 limit 10 offset 2
create view vd_7 as select id from vd_t where id in (1, 2, 3) and t like 'a%' and d is not null
create view vd_8 as select id, case when b then 'yes' when id < 0 then 'neg' else null end as k from vd_t
create view vd_9 as select distinct t from vd_t union select w from vd_u
create view vd_10 as select id, (select max(t_id) from vd_u u where u.t_id = vd_t.id) as mx from vd_t
create view vd_11 as select id, t::int as ti, d + 1 as tomorrow, now() as ts, 'lit' as l, 42 as num, -3 as neg from vd_t
create view vd_12 as select a.id from vd_t a left join vd_u b using (id) where b.w is null
create view vd_13 as with c as (select id from vd_t) select id from c
create view vd_14 as select id, row_number() over (partition by v order by id desc) as rn from vd_t
create view vd_15 as select id from vd_t where exists (select 1 from vd_u where vd_u.t_id = vd_t.id) and not b
create view vd_16 as select id, n between 1 and 2 as bt, t is distinct from 'a' as dist, array[1,2] as arr from vd_t
select viewname, definition from pg_views where viewname like 'vd_%' order by length(viewname), viewname
select viewname, pg_get_viewdef(viewname::regclass, true) from pg_views where viewname like 'vd_%' order by length(viewname), viewname
select pg_get_viewdef('vd_8'::regclass)
create table ve_t (id int primary key, v varchar(10), c char(3), t text, n numeric(8,2), f float8, i8 bigint, d date, ts timestamptz, b bool)
create table ve_u (id int, v varchar(10), w text)
create view ve_1 as select a.v = b.v as same, c = 'ab' as cq, i8 > a.id as big, f > 1 as fl, d > '2020-01-01' as dq from ve_t a join ve_u b on a.id = b.id
create view ve_2 as select id from ve_t where v in ('a', 'b') and id not in (4, 5) and b is true
create view ve_3 as select case id when 1 then 'one' when 2 then 'two' end as w, t || '-' || v as cat from ve_t
create view ve_4 as select s.x from (select id * 2 as x from ve_t) s where s.x > 3
create view ve_5 as with a as (select id from ve_t), b as (select id from ve_u) select a.id from a join b on a.id = b.id
create view ve_6 as select v, count(*) from ve_t group by 1 order by 2 desc
create view ve_7 as select distinct on (v) v, id from ve_t order by v, id
create view ve_8 as select x.id, y.id as yid from ve_t x cross join ve_t y
create view ve_9 as select ve_t.id, ve_u.w from ve_t, ve_u where ve_t.id = ve_u.id
create view ve_10 as select id, w from ve_4 join ve_u on ve_u.id = ve_4.x
create view ve_11 as select id::numeric(5,1) as n1, -1.5 as neg, true as tr, null as nu, now() - interval '1 day' as yest from ve_t
create view ve_12 as select id from ve_t union all select id from ve_u
create view ve_13 as select id from ve_t intersect select id from ve_u except select id from ve_u
create view ve_14 as select id from ve_t order by id limit 5
create view ve_15 as select id, (select w from ve_u where ve_u.id = ve_t.id limit 1) as w, id in (select id from ve_u) as has from ve_t
create view ve_16 as select count(distinct v) as cv, sum(n) as sn, avg(id) as av, max(t) as mx from ve_t
create view ve_17 as select upper(v) as uv, length(c) as lc, lower(t) as lt, substr(t, 2) as st, coalesce(v, 'x') as cv from ve_t
create view ve_18 as select id from ve_t where t ilike '%a%' and v like 'b%' and not (id > 3 or id < 1)
create view ve_19 as select greatest(id, 3) as g, nullif(id, 0) as nz, i8 + id as s, n * id as p from ve_t
create view ve_20 as select id, rank() over w as r from ve_t window w as (order by id)
select viewname, definition from pg_views where viewname like 've_%' order by length(viewname), viewname
select viewname, pg_get_viewdef(viewname::regclass, true) from pg_views where viewname like 've_%' order by length(viewname), viewname
drop view vd_1, vd_2, vd_3, vd_4, vd_5, vd_6, vd_7, vd_8, vd_9, vd_10, vd_11, vd_12, vd_13, vd_14, vd_15, vd_16
drop table vd_t, vd_u
drop view ve_10
drop view ve_1, ve_2, ve_3, ve_4, ve_5, ve_6, ve_7, ve_8, ve_9, ve_11, ve_12, ve_13, ve_14, ve_15, ve_16, ve_17, ve_18, ve_19, ve_20
drop table ve_t, ve_u
