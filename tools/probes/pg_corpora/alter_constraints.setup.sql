DROP TABLE IF EXISTS at2
DROP TABLE IF EXISTS at1
DROP TABLE IF EXISTS atc
CREATE TABLE at1 (id int, a int, b text)
INSERT INTO at1 VALUES (1, 1, 'x'), (2, 1, 'y'), (3, NULL, NULL)
CREATE TABLE at2 (x int)
INSERT INTO at2 VALUES (1), (5)
CREATE TABLE atc (a int, b int, v text)
INSERT INTO atc VALUES (1, 1, 'p'), (1, 2, 'q')
