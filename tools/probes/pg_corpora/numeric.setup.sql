DROP TABLE IF EXISTS t9
CREATE TABLE t9 (id int PRIMARY KEY, a int, b bigint, c numeric(10,3), d real, e double precision, f smallint)
INSERT INTO t9 VALUES (1, 2147483647, 9223372036854775807, 123.456, 1.5, 1.5, 32767)
