DROP TABLE IF EXISTS lbt
CREATE TABLE lbt (id int, a int[])
INSERT INTO lbt VALUES (1, '{1,2}'), (2, '[0:1]={5,6}'), (3, '[-2:-1][1:2]={{1,2},{3,4}}')
