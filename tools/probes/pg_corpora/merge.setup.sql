DROP TABLE IF EXISTS mt
DROP TABLE IF EXISTS ms
CREATE TABLE mt (id int primary key, v text, n int default 7)
CREATE TABLE ms (id int, v text)
INSERT INTO mt VALUES (1,'a',1),(2,'b',2),(3,'c',3)
INSERT INTO ms VALUES (1,'A'),(2,'B'),(4,'D'),(5,NULL)
DROP TABLE IF EXISTS mnk
CREATE TABLE mnk (a int, b text)
INSERT INTO mnk VALUES (1, 'x'), (2, NULL)
