DROP TABLE IF EXISTS sq_emp;
DROP TABLE IF EXISTS sq_dept;
CREATE TABLE sq_dept (id int primary key, name text, budget int);
CREATE TABLE sq_emp (id int primary key, dept_id int, name text, salary int);
INSERT INTO sq_dept VALUES (1, 'eng', 1000), (2, 'sales', 500), (3, 'empty', 0);
INSERT INTO sq_emp VALUES (1, 1, 'ann', 100), (2, 1, 'bob', 200), (3, 2, 'cat', 150), (4, 2, 'dan', NULL);
