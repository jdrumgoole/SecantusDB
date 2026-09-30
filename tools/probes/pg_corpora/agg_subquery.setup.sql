DROP TABLE IF EXISTS cd
DROP TABLE IF EXISTS ce
CREATE TABLE cd (id int PRIMARY KEY, name text, budget int)
CREATE TABLE ce (id int PRIMARY KEY, dept_id int, name text, salary int)
INSERT INTO cd VALUES (1, 'eng', 500), (2, 'sales', 300), (3, 'empty', 100), (4, 'nulls', NULL)
INSERT INTO ce VALUES (1, 1, 'ann', 200), (2, 1, 'bob', 250), (3, 2, 'cat', 100), (4, 2, 'dan', NULL), (5, NULL, 'eve', 50)
