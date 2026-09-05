DROP TABLE IF EXISTS sc21
CREATE TABLE sc21 (id int PRIMARY KEY, arr int[], j jsonb, s text)
INSERT INTO sc21 VALUES (1,'{1,2,3}','{"a":{"b":[1,2,3]}}','x,y'),(2,'{}','{"a":{"b":[]}}','z')
