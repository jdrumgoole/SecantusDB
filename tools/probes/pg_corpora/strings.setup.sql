DROP TABLE IF EXISTS s9t
CREATE TABLE s9t (id int PRIMARY KEY, t text, c varchar(10))
INSERT INTO s9t VALUES (1, 'Hello World', 'abc'), (2, '  pad  ', 'xyz'), (3, '', NULL)
