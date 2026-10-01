# An aggregate FILTER holding a subquery (batch 24): rewritten to the CASE
# argument it equals, for an aggregate that skips NULL inputs.
select count(*) filter (where exists (select 1 from fs24_t t where t.x = o.a)) from fs24_o o;
select count(*) filter (where o.a in (select x from fs24_t)) from fs24_o o;
select count(*) filter (where o.a not in (select x from fs24_t where x is not null)) from fs24_o o;
select sum(o.b) filter (where exists (select 1 from fs24_t t where t.x = o.a and t.f)) from fs24_o o;
select avg(o.a) filter (where o.b = (select min(y) from fs24_t)) from fs24_o o;
select min(o.id) filter (where o.a > (select avg(x) from fs24_t)), max(o.id) filter (where o.a > (select avg(x) from fs24_t)) from fs24_o o;
select o.b, count(*) filter (where exists (select 1 from fs24_t t where t.x = o.a)) from fs24_o o group by o.b order by o.b nulls first;
select string_agg(o.id::text, ',' order by o.id) filter (where o.a in (select x from fs24_t where f)) from fs24_o o;
select count(distinct o.a) filter (where o.a in (select x from fs24_t)) from fs24_o o;
select bool_and(o.a > 5) filter (where exists (select 1 from fs24_t t where t.y = o.b)) from fs24_o o;
select count(*) filter (where (select max(t.x) from fs24_t t where t.y = o.b) > 15) from fs24_o o;
select count(*) filter (where exists (select 1 from fs24_t t where t.x = o.a)) from fs24_o o where false;
select o.b from fs24_o o group by o.b having count(*) filter (where o.a in (select x from fs24_t)) > 0 order by 1 nulls first;
