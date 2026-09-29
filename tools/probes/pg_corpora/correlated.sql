SELECT d.id FROM cd d WHERE EXISTS (SELECT 1 FROM ce e WHERE e.dept_id = d.id) ORDER BY 1
SELECT d.id FROM cd d WHERE NOT EXISTS (SELECT 1 FROM ce e WHERE e.dept_id = d.id) ORDER BY 1
SELECT d.name, (SELECT sum(salary) FROM ce e WHERE e.dept_id = d.id) FROM cd d ORDER BY d.id
SELECT d.name, (SELECT max(e.salary) FROM ce e WHERE e.dept_id = d.id AND e.salary < d.budget) FROM cd d ORDER BY d.id
SELECT e.name FROM ce e WHERE e.salary > (SELECT avg(salary) FROM ce x WHERE x.dept_id = e.dept_id) ORDER BY 1
SELECT e.name FROM ce e WHERE e.salary = (SELECT max(salary) FROM ce x WHERE x.dept_id = e.dept_id) ORDER BY 1
SELECT d.id FROM cd d WHERE d.budget > ALL (SELECT salary FROM ce e WHERE e.dept_id = d.id) ORDER BY 1
SELECT d.id FROM cd d WHERE 200 = ANY (SELECT salary FROM ce e WHERE e.dept_id = d.id) ORDER BY 1
SELECT d.id FROM cd d WHERE d.id IN (SELECT e.dept_id FROM ce e WHERE e.salary > d.budget / 3) ORDER BY 1
SELECT d.id FROM cd d WHERE 100 NOT IN (SELECT salary FROM ce e WHERE e.dept_id = d.id) ORDER BY 1
SELECT d.id, ARRAY(SELECT e.name FROM ce e WHERE e.dept_id = d.id ORDER BY e.name) FROM cd d ORDER BY 1
SELECT d.id FROM cd d ORDER BY (SELECT count(*) FROM ce e WHERE e.dept_id = d.id), d.id
SELECT d.id, CASE WHEN EXISTS (SELECT 1 FROM ce e WHERE e.dept_id = d.id) THEN 'has' ELSE 'none' END FROM cd d ORDER BY 1
SELECT d.id, (SELECT e.name FROM ce e WHERE e.dept_id = d.id) FROM cd d ORDER BY 1
SELECT d.id FROM cd d WHERE EXISTS (SELECT 1 FROM ce e WHERE e.dept_id = d.id AND EXISTS (SELECT 1 FROM cd x WHERE x.id = e.dept_id AND x.budget > 400)) ORDER BY 1
SELECT d.id FROM cd d WHERE EXISTS (SELECT 1 FROM ce e WHERE e.dept_id = id) ORDER BY 1
SELECT d.id FROM cd d WHERE (SELECT count(*) FROM ce WHERE dept_id = d.id) >= 2 ORDER BY 1
SELECT d.name, count(*) FROM cd d JOIN ce e ON e.dept_id = d.id GROUP BY d.name HAVING count(*) = (SELECT count(*) FROM ce x WHERE x.dept_id = min(d.id)) ORDER BY 1
SELECT e.name, (SELECT d.name FROM cd d WHERE d.id = e.dept_id) FROM ce e JOIN cd dd ON dd.id = e.dept_id ORDER BY e.id
SELECT d.id FROM cd d WHERE EXISTS (SELECT 1 FROM ce e WHERE e.dept_id = d.id AND e.nosuch = 1)
UPDATE cd SET budget = (SELECT sum(salary) FROM ce e WHERE e.dept_id = cd.id) WHERE id <= 2 RETURNING id, budget
DELETE FROM ce WHERE NOT EXISTS (SELECT 1 FROM cd d WHERE d.id = ce.dept_id) RETURNING id
SELECT count(*) FROM ce
SELECT (SELECT id FROM cd WHERE false), (SELECT name FROM cd WHERE false)
