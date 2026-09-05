DROP TABLE IF EXISTS jr
CREATE TABLE jr (id int PRIMARY KEY, a int, b text, j json, jb jsonb)
INSERT INTO jr VALUES (1, 10, 'x', '{"k":1}', '{"k":1}'),(2, 20, 'y', '{"k":2}', '{"k":2}')
