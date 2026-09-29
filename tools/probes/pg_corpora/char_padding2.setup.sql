DROP TABLE IF EXISTS bp2
CREATE TABLE bp2 (id int PRIMARY KEY, c char(5), v varchar(5))
INSERT INTO bp2 VALUES (1, 'x'::char(5), 'x'), (2, 'y   ', 'y   '), (3, 'z', 'z')
UPDATE bp2 SET c = 'z'::char(5) WHERE id = 3
INSERT INTO bp2 SELECT 4, c, v FROM bp2 WHERE id = 1
