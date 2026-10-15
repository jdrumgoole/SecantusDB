# reference-version: 15
# With no ORDER BY of its own, a query with windows returns its rows in the order of the LAST window PostgreSQL evaluates: the windows run in common_prefix_cmp order (by sortgroupref, assigned to the statement ORDER BY first, then each window's ORDER BY before its PARTITION BY, named windows first), each over a sort its predecessor does not already satisfy. Ties go where PostgreSQL's quicksort leaves them.
select id, row_number() over (order by v desc) from wo_t
select id, row_number() over (order by v) from wo_t
select id, row_number() over (order by v nulls first) from wo_t
select id, row_number() over (order by v desc nulls last) from wo_t
select id, row_number() over (partition by g order by v) from wo_t
select id, row_number() over (partition by g) from wo_t
select id, row_number() over () from wo_t
select id, sum(v) over () from wo_t
select id, row_number() over (order by v), rank() over (partition by g order by id) from wo_t
select id, row_number() over (partition by g order by id), rank() over (order by v) from wo_t
select id, row_number() over (order by v) a, row_number() over (order by g) b from wo_t
select id, row_number() over (order by g) a, row_number() over (order by v) b from wo_t
select id, row_number() over (order by g) a, row_number() over (order by v desc) b from wo_t
select id, row_number() over (order by v) a, row_number() over (order by v desc) b from wo_t
select id, row_number() over (order by v desc) a, row_number() over (order by v) b from wo_t
select id, row_number() over (order by v nulls first) a, row_number() over (order by v) b from wo_t
select id, row_number() over (partition by g order by g desc) from wo_t
select id, row_number() over (partition by g, v), row_number() over (partition by v, g) from wo_t
select id, row_number() over (order by v), sum(v) over (order by v, id) from wo_t
select id, row_number() over (order by v, id), sum(v) over (order by v) from wo_t
select id, row_number() over (), sum(v) over (order by v) from wo_t
select id, sum(v) over (order by v), row_number() over () from wo_t
select id, sum(v) over (order by v rows between unbounded preceding and current row), sum(v) over (order by v) from wo_t
select id, row_number() over w1, rank() over w1 from wo_t window w1 as (order by s)
select id, row_number() over (order by v * -1) from wo_t
select id, row_number() over (order by coalesce(v, 0) desc, id) from wo_t
select id, row_number() over (order by s desc nulls first) from wo_t
select id, row_number() over (order by g), lag(id) over (order by g) from wo_t
select id, row_number() over (order by g) from wo_t order by id
select id, row_number() over (order by g) from wo_t order by g
select id, row_number() over (order by g) a, row_number() over (order by v) b from wo_t order by v
select id, row_number() over (order by v) from wo_t limit 3
select id, row_number() over (order by v desc) from wo_t limit 3 offset 2
select id, row_number() over (order by g) from wo_big
select id, row_number() over (order by g, v) from wo_big
select id, row_number() over (partition by v order by g) from wo_big
select id, rank() over (order by g), row_number() over (order by v) from wo_big
select * from (select id, row_number() over (order by v desc) r from wo_t) s
select * from (select id, row_number() over (order by v desc) r from wo_t) s where r < 5
select id from (select id, row_number() over (order by v desc) r from wo_t) s
select g, sum(sum(v)) over (order by g desc) from wo_t group by g
select count(*) over (), id from wo_t where g = 2
select id, first_value(id) over (partition by g order by v desc) from wo_t
select id, v, row_number() over (order by v desc) from wo_t where v is not null
select id, row_number() over b, row_number() over a from wo_t window a as (order by v), b as (order by g)
select id, row_number() over a, row_number() over b from wo_t window a as (order by v), b as (order by g)
select id, row_number() over a, row_number() over b from wo_t window b as (order by g), a as (order by v)
select id, sum(v) over (order by g rows 1 preceding), sum(v) over (order by g) from wo_t
select id, row_number() over (order by g), row_number() over (order by g desc) from wo_t
select id, row_number() over (order by g desc), row_number() over (order by g) from wo_t
select id, row_number() over (partition by g), row_number() over (order by v) from wo_t
select id, row_number() over (order by v), row_number() over (partition by g) from wo_t
select id, row_number() over (partition by g order by v), row_number() over (partition by g) from wo_t
select id, row_number() over (partition by g), row_number() over (partition by g order by v) from wo_t
select id, row_number() over (order by g, v), row_number() over (order by g) from wo_t
select id, row_number() over (order by g), row_number() over (order by g, v) from wo_t
select id, row_number() over (order by v), row_number() over (order by g) from wo_t order by g
select v, row_number() over (order by v) from wo_t order by v
select id, row_number() over (order by s) from wo_t
select id, row_number() over (order by s collate "C") from wo_t
select id, row_number() over (order by upper(s)) from wo_t
select id, row_number() over (order by v + 0), row_number() over (order by v) from wo_t
select id, row_number() over (order by g % 2, id desc) from wo_t
select id, row_number() over (order by v) from (select * from wo_t order by id desc) s
with c as (select id, row_number() over (order by v desc) r from wo_t) select * from c
with c as (select id, row_number() over (order by v desc) r from wo_t) select id from c
select w.id, row_number() over (order by w.v desc) from wo_t w join wo_t w2 on w2.id = w.id
select w.id, row_number() over (partition by w.g order by w2.v) from wo_t w join wo_t w2 on w2.id = 9 - w.id
select id, row_number() over (order by v desc) from wo_t union all select id, 0 from wo_t where id < 3
select id, row_number() over (order by v desc) from wo_t where g in (1, 2)
select id, ntile(3) over (order by v), percent_rank() over (order by v) from wo_t
select id, row_number() over (partition by g order by v), sum(v) over (partition by g), count(*) over () from wo_t
select id, row_number() over (order by g) from wo_big limit 10
select id, row_number() over (order by g desc) from wo_big where v = 1
select id, sum(id) over (partition by g order by v range between unbounded preceding and unbounded following) from wo_big
select id, row_number() over (partition by v % 2 order by g) from wo_big
select id, v, dense_rank() over (order by v nulls first), row_number() over (order by id desc) from wo_t
select id, row_number() over (order by id desc), dense_rank() over (order by v nulls first) from wo_t
