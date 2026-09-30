WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT sum(n) FROM r
WITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT n FROM r ORDER BY n
WITH RECURSIVE t(id, name, depth) AS (SELECT id, name, 0 FROM rt_emp WHERE boss IS NULL UNION ALL SELECT e.id, e.name, t.depth + 1 FROM rt_emp e JOIN t ON e.boss = t.id) SELECT name, depth FROM t ORDER BY depth, name
WITH RECURSIVE up(id, boss) AS (SELECT id, boss FROM rt_emp WHERE id = 5 UNION SELECT e.id, e.boss FROM rt_emp e, up WHERE e.id = up.boss) SELECT id FROM up ORDER BY id
WITH RECURSIVE c(x) AS (SELECT 1 UNION SELECT (x % 3) + 1 FROM c) SELECT x FROM c ORDER BY x
WITH RECURSIVE f(n, fact) AS (SELECT 1, 1::numeric UNION ALL SELECT n + 1, fact * (n + 1) FROM f WHERE n < 20) SELECT fact::text FROM f WHERE n = 20
WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 3), s AS (SELECT n * 10 AS m FROM r) SELECT m FROM s ORDER BY m
WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 4) SELECT count(*) FROM rt_emp WHERE id IN (SELECT n FROM r)
WITH RECURSIVE r(n) AS (SELECT n FROM r) SELECT * FROM r
WITH RECURSIVE r(n) AS (SELECT 1 WHERE false UNION ALL SELECT n + 1 FROM r) SELECT count(*) FROM r
WITH RECURSIVE p(path, id) AS (SELECT name, id FROM rt_emp WHERE id = 1 UNION ALL SELECT p.path || '/' || e.name, e.id FROM rt_emp e JOIN p ON e.boss = p.id) SELECT path FROM p ORDER BY path
WITH r AS (SELECT 1 AS n) SELECT n FROM r
