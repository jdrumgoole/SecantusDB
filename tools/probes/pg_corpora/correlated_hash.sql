# Correlated subqueries whose outer references are all equalities run ONCE
# and are answered by a hash lookup (batch 30, semijoin_hash.rs). Each line is
# an edge a hash could get wrong: NULL keys, int against float / numeric,
# -0, text case, dates, empty aggregates, LIMIT / OFFSET, NOT IN, ANY / ALL.
select id from ch30_o o where exists (select 1 from ch30_t t where t.x = o.a) order by id;
select id from ch30_o o where not exists (select 1 from ch30_t t where t.x = o.a) order by id;
select id from ch30_o o where exists (select 1 from ch30_t t where t.x = o.a and t.y = o.b) order by id;
select id from ch30_o o where exists (select 1 from ch30_t t where o.d = t.d) order by id;
select id from ch30_o o where exists (select 1 from ch30_t t where t.g = o.f) order by id;
select id from ch30_o o where exists (select 1 from ch30_t t where t.n = o.n) order by id;
select id from ch30_o o where exists (select 1 from ch30_t t where t.x = o.f) order by id;
select id from ch30_o o where exists (select 1 from ch30_t t where t.y = o.b and t.v > 15) order by id;
select id, (select count(*) from ch30_t t where t.x = o.a) from ch30_o o order by id;
select id, (select sum(v) from ch30_t t where t.x = o.a) from ch30_o o order by id;
select id, (select max(t.v) from ch30_t t where t.y = o.b) from ch30_o o order by id;
select id, (select avg(v) from ch30_t t where t.d = o.d) from ch30_o o order by id;
select id, (select t.v from ch30_t t where t.x = o.a limit 1) from ch30_o o order by id;
select id, (select t.v from ch30_t t where t.x = o.a limit 1 offset 1) from ch30_o o order by id;
select id, array(select t.id from ch30_t t where t.x = o.a) from ch30_o o order by id;
select id from ch30_o o where o.id in (select t.id from ch30_t t where t.x = o.a) order by id;
select id from ch30_o o where o.id not in (select t.v from ch30_t t where t.x = o.a) order by id;
select id from ch30_o o where 15 < all (select t.v from ch30_t t where t.x = o.a) order by id;
select id from ch30_o o where 15 < any (select t.v from ch30_t t where t.x = o.a) order by id;
select id, (select t.v from ch30_t t where t.x = o.a) from ch30_o o where o.id <> 1 order by id;
select id, (select t.v from ch30_t t where t.x = o.a) from ch30_o o order by id;
update ch30_o o set b = 'z' where exists (select 1 from ch30_t t where t.x = o.a and t.v = 50) returning id;
select id, b from ch30_o order by id;
