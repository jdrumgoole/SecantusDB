DROP TABLE IF EXISTS eu15;
CREATE TABLE eu15 (id int primary key, t text, a int, b int);
CREATE UNIQUE INDEX eu15_lt ON eu15 (lower(t));
