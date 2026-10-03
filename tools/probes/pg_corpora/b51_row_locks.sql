# reference-version: 15
# Locking clauses (batch 51): FOR UPDATE / NO KEY UPDATE / SHARE / KEY SHARE over
# joins, FROM-subqueries and views, and the 0A000 / 42P01 checks PostgreSQL
# makes on them. Multi-session behaviour is in the slice tests.
select p.n, c.m from b51_p p join b51_c c on c.pid = p.id order by p.id for update;
select p.n, c.m from b51_p p join b51_c c on c.pid = p.id order by p.id for update of p;
select p.n, c.m from b51_p p join b51_c c on c.pid = p.id order by p.id for share of c nowait;
select p.n, c.m from b51_p p, b51_c c where c.pid = p.id order by p.id for key share;
select p.n from b51_p p left join b51_c c on c.pid = p.id order by p.id for update;
select p.n from b51_p p left join b51_c c on c.pid = p.id order by p.id for update of p;
select p.n from b51_p p left join b51_c c on c.pid = p.id order by p.id for share of c;
select c.m from b51_p p right join b51_c c on c.pid = p.id order by c.id for update of c;
select c.m from b51_p p right join b51_c c on c.pid = p.id order by c.id for no key update of p;
select c.m from b51_p p full join b51_c c on c.pid = p.id order by c.id for key share;
select count(*) from b51_p for update;
select distinct n from b51_p for update;
select n from b51_p group by n for share;
select n from b51_p having n > 1 for update;
select * from b51_p for update of zz;
select * from b51_p for key share of b51_p, zz;
select * from b51_p x for update of b51_p;
select * from b51_p union select * from b51_p for update;
select * from (select count(*) from b51_p) s for update;
select * from (select distinct n from b51_p) s for share;
select * from (select count(*) from b51_p) s, b51_c c order by c.id for update of c;
select row_number() over () from b51_p for update;
select sum(n) over () from b51_p for key share;
select * from (select * from b51_p where id = 1) s for update;
select * from (select id, n from b51_p) s where s.n > 15 order by id for share;
select n from b51_v where id = 2 for update;
select n from b51_v order by id for key share nowait;
select * from b51_va for update;
with w as (select * from b51_p) select * from w order by id for update;
select * from b51_p for update for share;
select * from b51_p order by id for share skip locked;
select * from b51_p order by id limit 1 for no key update skip locked;
