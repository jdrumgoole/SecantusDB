# reference-version: 15
# Batch 55: an ordering filter over a COLLATED inner text column, and a
# numeric min / max under a filter, are hashed (run once). Every answer
# must be the per-row path's -- and PostgreSQL's.
select id from b55_o o where exists (select 1 from b55_i i where i.x = o.a and i.s > o.s) order by 1
select id from b55_o o where exists (select 1 from b55_i i where i.x = o.a and i.s < o.s) order by 1
select id from b55_o o where exists (select 1 from b55_i i where i.x = o.a and i.s >= o.s) order by 1
select id from b55_o o where exists (select 1 from b55_i i where i.x = o.a and i.s <> o.s) order by 1
select id, (select count(*) from b55_i i where i.x = o.a and i.s <= o.s) from b55_o o order by 1
select id, (select count(*) from b55_i i where i.x = o.a and i.c > o.s) from b55_o o order by 1
select id, (select count(*) from b55_i i where i.x = o.a and i.u < o.s) from b55_o o order by 1
select id, (select count(*) from b55_i i where i.x = o.a and i.p > o.s) from b55_o o order by 1
select id, (select max(p) from b55_i i where i.x = o.a and i.s > o.s) from b55_o o order by 1
select id, (select min(v) from b55_i i where i.x = o.a and i.w > o.n) from b55_o o order by 1
select id, (select max(v) from b55_i i where i.x = o.a and i.w > o.n) from b55_o o order by 1
select id, (select max(v) from b55_i i where i.x = o.a and i.w <= o.n) from b55_o o order by 1
select id, (select min(w) from b55_i i where i.x = o.a and i.v < o.n) from b55_o o order by 1
select id, (select max(w) from b55_i i where i.x = o.a and i.v <> o.n) from b55_o o order by 1
select id, (select min(v) from b55_i i where i.x = o.a and i.s > o.s) from b55_o o order by 1
select id, (select pg_typeof(max(v)) from b55_i i where i.x = o.a and i.w > o.n) from b55_o o order by 1
select id, (select max(v)::text from b55_i i where i.x = o.a and i.w > o.n) from b55_o o order by 1
