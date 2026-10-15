DROP TABLE IF EXISTS iss_src, iss_dst, iss_s, iss_d, iss_e CASCADE
create table iss_src (c0 int2, c1 int4, c2 int8, c3 numeric, c4 float4, c5 float8, c6 text, c7 varchar, c8 varchar(3), c9 bpchar, c10 char(3), c11 "char", c12 name, c13 bool, c14 date, c15 time, c16 timetz, c17 timestamp, c18 timestamptz, c19 interval, c20 json, c21 jsonb, c22 uuid, c23 bytea, c24 inet, c25 cidr, c26 macaddr, c27 money, c28 bit(4), c29 varbit, c30 oid, c31 int4[], c32 text[], c33 int4range, c34 point, c35 tsvector)
create table iss_dst (c0 int2, c1 int4, c2 int8, c3 numeric, c4 float4, c5 float8, c6 text, c7 varchar, c8 varchar(3), c9 bpchar, c10 char(3), c11 "char", c12 name, c13 bool, c14 date, c15 time, c16 timetz, c17 timestamp, c18 timestamptz, c19 interval, c20 json, c21 jsonb, c22 uuid, c23 bytea, c24 inet, c25 cidr, c26 macaddr, c27 money, c28 bit(4), c29 varbit, c30 oid, c31 int4[], c32 text[], c33 int4range, c34 point, c35 tsvector)
create table iss_s (i int, i8 int8, t text, n numeric, f float8, f4 float4, b bool, d date, ts timestamp, tz timestamptz, j json, a int4[], ta text[], c "char", v varchar)
insert into iss_s values (1, 9000000000, 'abcd', 2.5, 3.5, 1.25, true, '2020-01-02', '2020-01-02 03:04:05.678', '2020-01-02 23:30:00+00', '{"a": 1}', '{1,2}', '{x,y}', 'Q', 'ab  ')
insert into iss_s values (-3, 70000, 'ab  ', -2.5, -0.5, 2.5, false, '1999-12-31', '1999-12-31 00:00:00', '1999-12-31 00:00:00+00', '[1, 2]', '{3}', '{z}', 'R', 'abc')
create table iss_d (i int, i2 int2, i8 int8, n numeric, n51 numeric(5,1), f float8, f4 float4, t text, v3 varchar(3), c3 char(3), b bool, d date, ts timestamp, tz timestamptz, j jsonb, a8 int8[], at text[], ch "char", nm name)
create table iss_e (k int primary key, v int, t text)
insert into iss_e values (1, 10, 'x')
