# reference-version: 15
# Rows that tie under ORDER BY, in the order PostgreSQL 15's sort leaves them (RP-028): its quicksort is not stable from 7 rows up, a LIMIT over the Sort bounds it to a heap once more than twice the bound has arrived, and an aggregate's ORDER BY / a join's ORDER BY go through the same sort. Not here: grouped output order (HashAggregate), DISTINCT ON (kept stable), and FETCH ... WITH TIES.
select id from ot_t order by v
select id from ot_t order by v desc
select id from ot_t order by v nulls first
select id from ot_t order by v desc nulls last
select id, s from ot_t order by s
select id from ot_t order by s, v
select id from ot_t order by v limit 3
select id from ot_t order by v limit 4
select id from ot_t order by v limit 5
select id from ot_t order by v limit 1
select id from ot_t order by v limit 2 offset 1
select id from ot_t order by v offset 2
select id from ot_t order by v limit 0
select id from ot_t where v is not null order by v
select id from ot_t where id > 1 order by v limit 2
select id from ot_t order by v + 0
select id from ot_t order by coalesce(v, 0)
select id from ot_t order by 1 = 1
select id, v from ot_t order by 2
select id from ot_u order by g
select id from ot_u order by g desc
select id from ot_u order by g, h
select id from ot_u order by g limit 5
select id from ot_u order by g limit 25
select id from ot_u order by g limit 30
select id from ot_u order by g limit 31
select id from ot_u order by h limit 7 offset 3
select id from ot_w order by k
select id from ot_w order by k limit 10
select id from ot_w order by k desc limit 33
select id from ot_w order by k limit 99
select id from ot_w order by k limit 100
select string_agg(id::text, ',' order by v) from ot_t
select array_agg(id order by v) from ot_t
select id, row_number() over (order by v) from ot_t order by v
select id, row_number() over (order by v) from ot_t order by v limit 3
select id, sum(id) over (partition by g) from ot_u order by g limit 8
select id from (select id, v from ot_t order by v) s
select id from (select id, v from ot_t order by v limit 4) s
select id from ot_t union all select id from ot_t order by 1 limit 3
select v, count(*) from ot_t group by v order by 2
select t.id from ot_t t join ot_t x on t.id = x.id order by t.v
select t.id from ot_t t join ot_t x on t.id = x.id order by t.v limit 2
select id from ot_t order by v for update
select id from ot_t order by v limit 2 for update
select t.id from ot_t t left join ot_t x on t.id = x.id order by t.v
select t.id, x.id from ot_t t, ot_t x where t.id = x.id and t.id < 9 order by t.v desc
select t.id from ot_t t join ot_u x on t.id = x.id order by x.g
select t.id from ot_t t join ot_u x on t.id = x.id order by x.g limit 2
