SELECT d.name, count(*) FROM cd d JOIN ce e ON e.dept_id = d.id GROUP BY d.name HAVING count(*) = (SELECT count(*) FROM ce x WHERE x.dept_id = min(d.id)) ORDER BY 1
SELECT d.name, count(*) FROM cd d JOIN ce e ON e.dept_id = d.id GROUP BY d.name HAVING count(*) >= (SELECT count(*) FROM ce x WHERE x.name = d.name) ORDER BY 1
SELECT d.id, count(*) FROM cd d JOIN ce e ON e.dept_id = d.id GROUP BY d.id HAVING count(*) = (SELECT count(*) FROM ce x WHERE x.dept_id = d.id) ORDER BY 1
SELECT d.name, (SELECT count(*) FROM ce x WHERE x.dept_id = max(d.id)) FROM cd d GROUP BY d.name ORDER BY 1
SELECT d.name FROM cd d GROUP BY d.name HAVING EXISTS (SELECT 1 FROM ce x WHERE x.dept_id = min(d.id)) ORDER BY 1
SELECT lower(d.name) AS ln, count(*), (SELECT count(*) FROM ce x WHERE lower(x.name) < lower(d.name)) FROM cd d GROUP BY lower(d.name) ORDER BY 1
SELECT d.budget, count(*) FROM cd d GROUP BY d.budget HAVING count(*) < (SELECT count(*) FROM ce) ORDER BY 2 DESC, 1 NULLS LAST LIMIT 2
SELECT d.name, max(d.budget) AS mb, (SELECT max(salary) FROM ce x WHERE x.dept_id = d.id) FROM cd d GROUP BY d.name, d.id ORDER BY mb DESC NULLS LAST
SELECT count(*), (SELECT count(*) FROM ce) FROM cd
SELECT d.name, count(e.id) FROM cd d LEFT JOIN ce e ON e.dept_id = d.id GROUP BY d.name HAVING count(e.id) = (SELECT count(*) FROM ce x WHERE x.dept_id IN (SELECT y.id FROM cd y WHERE y.name = d.name)) ORDER BY 1
