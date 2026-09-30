DROP TABLE IF EXISTS rt_emp
CREATE TABLE rt_emp (id int PRIMARY KEY, boss int, name text)
INSERT INTO rt_emp VALUES (1, NULL, 'ceo'), (2, 1, 'cto'), (3, 1, 'cfo'), (4, 2, 'dev'), (5, 4, 'intern')
