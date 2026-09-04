DROP TABLE IF EXISTS sc
CREATE TABLE sc (id int PRIMARY KEY, j jsonb, arr int[], s text, n numeric, ts timestamptz)
INSERT INTO sc VALUES (1,'{"a":{"b":[1,2,3]},"c":"x"}','{1,2,3}','hello',1.50,'2020-01-05 10:00:00+00')
INSERT INTO sc VALUES (2,'{"a":{"b":[]},"c":null}','{}','world',2.50,'2021-06-30 23:59:59+00')
