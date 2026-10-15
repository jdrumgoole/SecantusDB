DROP TABLE IF EXISTS sm2
DROP TABLE IF EXISTS smu
CREATE TABLE sm2 (n numeric, c "char")
INSERT into sm2 values (1.5, 'a'), (2.5, 'z')
CREATE UNLOGGED TABLE smu (a int)
