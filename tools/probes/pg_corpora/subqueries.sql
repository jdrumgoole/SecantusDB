# --- scalar subquery, FROM-less ---
SELECT (SELECT 1)
SELECT (SELECT 1) + 2
SELECT (SELECT max(salary) FROM sq_emp)
SELECT (SELECT name FROM sq_dept WHERE id = 1)
SELECT (SELECT name FROM sq_dept WHERE id = 99)
# --- scalar subquery in the target list of a real scan ---
SELECT id, (SELECT count(*) FROM sq_emp) AS total FROM sq_dept ORDER BY id
# --- correlated scalar subquery ---
SELECT d.name, (SELECT count(*) FROM sq_emp e WHERE e.dept_id = d.id) AS n FROM sq_dept d ORDER BY d.id
SELECT e.name, (SELECT d.name FROM sq_dept d WHERE d.id = e.dept_id) AS dept FROM sq_emp e ORDER BY e.id
# --- EXISTS / NOT EXISTS ---
SELECT id FROM sq_dept d WHERE EXISTS (SELECT 1 FROM sq_emp e WHERE e.dept_id = d.id) ORDER BY id
SELECT id FROM sq_dept d WHERE NOT EXISTS (SELECT 1 FROM sq_emp e WHERE e.dept_id = d.id) ORDER BY id
SELECT 1 WHERE EXISTS (SELECT 1 FROM sq_dept)
SELECT 1 WHERE EXISTS (SELECT 1 FROM sq_dept WHERE id = 99)
# --- IN / NOT IN (subquery) ---
SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp) ORDER BY id
SELECT id FROM sq_dept WHERE id NOT IN (SELECT dept_id FROM sq_emp) ORDER BY id
SELECT name FROM sq_emp WHERE dept_id IN (SELECT id FROM sq_dept WHERE budget > 600) ORDER BY name
# NOT IN with a NULL in the subquery result: PostgreSQL returns zero rows
SELECT id FROM sq_dept WHERE id NOT IN (SELECT salary FROM sq_emp) ORDER BY id
# --- ANY / ALL ---
SELECT id FROM sq_dept WHERE id = ANY (SELECT dept_id FROM sq_emp) ORDER BY id
SELECT id FROM sq_dept WHERE budget > ALL (SELECT salary FROM sq_emp WHERE salary IS NOT NULL) ORDER BY id
SELECT id FROM sq_dept WHERE budget > ANY (SELECT salary FROM sq_emp WHERE salary IS NOT NULL) ORDER BY id
# --- subquery in FROM ---
SELECT y FROM (SELECT 1 AS y) s
SELECT s.n FROM (SELECT count(*) AS n FROM sq_emp) s
SELECT s.dept_id, s.c FROM (SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s ORDER BY s.dept_id
SELECT s.c FROM (SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s WHERE s.dept_id = 1
SELECT max(s.c) FROM (SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s
# --- subquery in FROM joined to a table ---
SELECT d.name, s.c FROM sq_dept d JOIN (SELECT dept_id, count(*) AS c FROM sq_emp GROUP BY dept_id) s ON s.dept_id = d.id ORDER BY d.name
# --- CTE ---
WITH c AS (SELECT 1 AS x) SELECT x FROM c
WITH c AS (SELECT id, name FROM sq_dept) SELECT name FROM c WHERE id = 1
WITH c AS (SELECT dept_id, count(*) AS n FROM sq_emp GROUP BY dept_id) SELECT * FROM c ORDER BY dept_id
WITH c AS (SELECT id FROM sq_dept WHERE budget > 400) SELECT count(*) FROM c
# --- two CTEs, and one referencing the other ---
WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS y) SELECT x, y FROM a, b
WITH a AS (SELECT id, budget FROM sq_dept), b AS (SELECT id FROM a WHERE budget > 400) SELECT count(*) FROM b
# --- CTE joined to a table ---
WITH c AS (SELECT dept_id, count(*) AS n FROM sq_emp GROUP BY dept_id) SELECT d.name, c.n FROM sq_dept d JOIN c ON c.dept_id = d.id ORDER BY d.name
# --- subquery inside a CTE ---
WITH c AS (SELECT id FROM sq_dept WHERE EXISTS (SELECT 1 FROM sq_emp e WHERE e.dept_id = sq_dept.id)) SELECT count(*) FROM c
# --- uncorrelated subquery inside a CTE body ---
WITH c AS (SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp)) SELECT count(*) FROM c
# --- subquery in HAVING, and in the select list of a grouped query ---
SELECT dept_id, count(*) FROM sq_emp GROUP BY dept_id HAVING count(*) > (SELECT 1) ORDER BY dept_id
# --- nested subqueries ---
SELECT (SELECT max(salary) FROM sq_emp WHERE dept_id IN (SELECT id FROM sq_dept WHERE budget > 600))
SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp WHERE salary > (SELECT 120)) ORDER BY id
# --- an empty subquery on each side of IN / NOT IN ---
SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp WHERE salary > 9999) ORDER BY id
SELECT id FROM sq_dept WHERE id NOT IN (SELECT dept_id FROM sq_emp WHERE salary > 9999) ORDER BY id
# --- ALL / ANY over an empty subquery ---
SELECT id FROM sq_dept WHERE budget > ALL (SELECT salary FROM sq_emp WHERE salary > 9999) ORDER BY id
SELECT id FROM sq_dept WHERE budget > ANY (SELECT salary FROM sq_emp WHERE salary > 9999) ORDER BY id
# --- a scalar subquery returning more than one row is 21000 ---
SELECT (SELECT id FROM sq_dept)
# --- subquery in UPDATE / DELETE WHERE ---
UPDATE sq_dept SET budget = 1 WHERE id IN (SELECT dept_id FROM sq_emp WHERE salary > 180)
SELECT id, budget FROM sq_dept ORDER BY id
DELETE FROM sq_emp WHERE dept_id IN (SELECT id FROM sq_dept WHERE budget = 1)
SELECT count(*) FROM sq_emp
# --- scalar subquery compared against a column ---
SELECT name FROM sq_emp WHERE salary = (SELECT max(salary) FROM sq_emp)
# --- ARRAY(subquery) ---
SELECT ARRAY(SELECT id FROM sq_dept ORDER BY id)
# --- a subquery over a subquery in FROM ---
SELECT count(*) FROM (SELECT id FROM sq_dept WHERE id IN (SELECT dept_id FROM sq_emp)) s
# --- an undefined column inside a subquery keeps its 42703 ---
SELECT id FROM sq_dept WHERE id IN (SELECT nosuchcol FROM sq_emp)
WITH big AS (SELECT id FROM sq_emp WHERE salary > 120) SELECT id FROM sq_emp WHERE id IN (SELECT id FROM big) ORDER BY id
WITH big AS (SELECT id FROM sq_emp WHERE salary > 120) SELECT id, (SELECT count(*) FROM big) FROM sq_dept ORDER BY id
WITH d AS (SELECT id FROM sq_dept) SELECT id FROM sq_dept x WHERE EXISTS (SELECT 1 FROM d WHERE d.id = x.id AND x.budget > 100) ORDER BY id
