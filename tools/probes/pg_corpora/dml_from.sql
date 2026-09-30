UPDATE df_t SET n = df_s.v FROM df_s WHERE df_t.id = df_s.tid RETURNING id, n
SELECT id, n FROM df_t ORDER BY id
UPDATE df_t t SET n = s.v + t.n FROM df_s s WHERE t.id = s.tid AND s.v > 150 RETURNING t.id, t.n
SELECT id, n FROM df_t ORDER BY id
UPDATE df_t SET s = u.w, n = n + 1 FROM df_u u, df_s WHERE df_t.id = u.id AND df_s.id = u.id RETURNING id, n, s
SELECT id, n, s FROM df_t ORDER BY id
UPDATE df_t SET n = 0 FROM df_s WHERE df_s.tid = 99
SELECT id, n FROM df_t ORDER BY id
UPDATE df_t SET n = x.v FROM (SELECT 4 AS id, 44 AS v) x WHERE df_t.id = x.id
SELECT id, n FROM df_t ORDER BY id
UPDATE df_t SET (n, s) = (5, 'five') WHERE id = 4 RETURNING id, n, s
UPDATE df_t SET (n, s) = (5) WHERE id = 4
UPDATE df_t SET n = df_s.v FROM df_s WHERE df_t.id = df_s.tid RETURNING df_s.v
DELETE FROM df_t USING df_u WHERE df_t.id = df_u.id RETURNING df_t.id
SELECT id FROM df_t ORDER BY id
DELETE FROM df_t t USING df_s s WHERE t.id = s.tid AND s.v = 200
SELECT id FROM df_t ORDER BY id
DELETE FROM df_t USING df_s WHERE df_s.id = 42
SELECT id FROM df_t ORDER BY id
create table ry_o (id int, n int)
insert into ry_o values (1, 10), (2, 20), (3, 30)
create view ry_v as select id, n from ry_o
select count(*) from ry_o where exists (select 1 from (SELECT r2.id AS o_id FROM ry_o r2 WHERE id = 1) AS s where ry_o.id = s.o_id)
select count(*) from ry_o where exists (select 1 from (SELECT ry_v.id AS o_id FROM ry_v WHERE id = 1) AS s where ry_o.id = s.o_id)
DELETE FROM ry_o USING (SELECT ry_v.id AS o_id FROM ry_v WHERE id = 1) AS s WHERE ry_o.id = s.o_id
select * from ry_o order by 1
UPDATE ry_o SET n = s.x FROM (SELECT r2.id AS i, r2.n * 2 AS x FROM ry_o r2 WHERE id = 2) AS s WHERE ry_o.id = s.i
select * from ry_o order by 1
drop view ry_v
drop table ry_o
