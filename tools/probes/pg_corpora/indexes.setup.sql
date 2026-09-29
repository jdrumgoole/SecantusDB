DROP VIEW IF EXISTS ixv
DROP TABLE IF EXISTS ix1
DROP TABLE IF EXISTS ix2
CREATE TABLE ix1 (id int PRIMARY KEY, a int, b text, c int, u text UNIQUE)
CREATE TABLE ix2 (id int PRIMARY KEY, s text)
INSERT INTO ix1 VALUES (1, 10, 'x', 1, 'p'), (2, 20, 'y', 2, 'q'), (3, 10, 'x', 3, NULL), (4, NULL, NULL, 4, NULL)
INSERT INTO ix2 VALUES (1, 'a'), (2, 'a')
