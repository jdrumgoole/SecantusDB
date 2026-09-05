DROP TABLE IF EXISTS ag
CREATE TABLE ag (id int PRIMARY KEY, g text, n int, x numeric, f float8, s text, b bool)
INSERT INTO ag VALUES (1,'a',1,1.5,1.5,'p',true),(2,'a',NULL,NULL,NULL,NULL,NULL),(3,'b',NULL,NULL,NULL,NULL,NULL)
DROP TABLE IF EXISTS empt
CREATE TABLE empt (n int, x numeric, s text)
