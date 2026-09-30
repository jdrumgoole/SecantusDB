# reference-version: 15
# EXPLAIN (COSTS OFF) detail: a scan's Index Cond / Filter, a join's Hash Cond /
# Join Filter, WHERE conjuncts pushed to their scan, aliases. (Which side a
# hash join builds is PostgreSQL's cost model; only same-shaped joins here.)
create table ex_a (id int primary key, v int, t text)
create table ex_b (id int, a_id int)
explain (costs off) select * from ex_a where v > 5
explain (costs off) select * from ex_a where id = 1
explain (costs off) select count(*) from ex_a group by v
explain (costs off) select * from ex_a order by t limit 3
explain (costs off) select * from ex_a where t like 'x%' and v < 3
explain (costs off, format json) select * from ex_a where v > 5
explain (costs off) update ex_a set v = 1 where v = 2
explain (costs off) delete from ex_a where id = 3
create table ex_c (id int, v int)
create table ex_d (id int, w int)
explain (costs off) select * from ex_c c join ex_d d on c.id = d.id where c.v > 1 and d.w < 2 and c.v < d.w
drop table ex_a, ex_b, ex_c, ex_d
