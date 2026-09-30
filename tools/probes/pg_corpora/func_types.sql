# reference-version: 15
# Built-in functions resolve by argument TYPE against PostgreSQL 15's own
# overloads (42883 when none takes them); a function result types an operand;
# a COALESCE literal is coerced when analysed; a derived table inside a
# subquery may read the enclosing row.
create table ty_t (id int, t text, d date, b bool, j jsonb)
insert into ty_t values (1, 'x', '2020-01-01', true, '{}')
select * from ty_t where (id + 1) = 'a'
select * from ty_t where upper(t) = 1
select * from (select id, t from ty_t) s where s.t = 1
with c as (select d from ty_t) select * from c where d = 1
select date_trunc(1, d) from ty_t
select extract(year from t) from ty_t
select length(d) from ty_t
select round(t) from ty_t
select jsonb_typeof(t) from ty_t
select array_length(t, 1) from ty_t
select lower(b) from ty_t
select coalesce(id, 'x') from ty_t
select abs(b) from ty_t
select substr(d, 1) from ty_t
select to_char(t, 'x') from ty_t
select id, (select max(x.n) from (select q.id as n from ty_t q where q.id <= ty_t.id) x) from ty_t
create table ty_u (id int, v int)
insert into ty_u values (1, 10), (2, 20), (3, 30)
select id, (select sum(x.v) from (select q.v from ty_u q where q.id <= ty_u.id) x) from ty_u order by 1
select id, (select count(*) from (select 1 from ty_u q where q.v > ty_u.v and exists (select 1 from ty_u z where z.id = q.id)) x) from ty_u order by 1
select id from ty_u where exists (select 1 from (select v from ty_u q where q.id = ty_u.id) x where x.v > 15) order by 1
drop table ty_u
drop table ty_t
