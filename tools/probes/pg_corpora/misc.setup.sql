DROP TABLE IF EXISTS mi12
CREATE TABLE mi12 (id int PRIMARY KEY, s text, n numeric, b bool, d date)
INSERT INTO mi12 VALUES (1,'a',1.5,true,'2020-01-01'), (2,NULL,NULL,NULL,NULL)
