DROP TABLE IF EXISTS p20
CREATE TABLE p20 (id int PRIMARY KEY, s text, n int, big bigint, x numeric(10,2), f float8, b bool, d date, ts timestamp, tz timestamptz, by bytea, arr int[], j jsonb)
INSERT INTO p20 VALUES (1,'alpha',10,100000000000,1.50,1.25,true,'2020-01-05','2020-01-05 10:20:30','2020-01-05 10:20:30+00','\x4142','{1,2,3}','{"a":1}')
INSERT INTO p20 VALUES (2,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)
