DROP TABLE IF EXISTS chc_t CASCADE
DROP TABLE IF EXISTS chc_e CASCADE
DROP TABLE IF EXISTS chc_d CASCADE
DROP TABLE IF EXISTS chc_n CASCADE
create table chc_t (i2 int2, i4 int4, i8 int8, n numeric, t text, v varchar, b bool, c "char")
insert into chc_t values (66, 67, 68, 69, 'XYZ', 'Q', true, 'Z'), (null, null, null, null, null, null, null, null)
create table chc_e (i2 int2, i4 int4, c "char")
create table chc_d (c "char")
CREATE TABLE chc_n (i int4, s smallint, t text, b bool)
