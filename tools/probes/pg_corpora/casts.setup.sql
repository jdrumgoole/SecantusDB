DROP TABLE IF EXISTS p15
CREATE TABLE p15 (id int PRIMARY KEY, s text, n int, d date, b bool, x numeric)
INSERT INTO p15 VALUES (1,'a',10,'2020-01-01',true,1.5),(2,NULL,NULL,NULL,NULL,NULL)
