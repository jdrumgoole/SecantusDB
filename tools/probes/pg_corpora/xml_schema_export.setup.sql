DROP SCHEMA IF EXISTS xse5 CASCADE
DROP SCHEMA IF EXISTS "Xse5:odd" CASCADE
DROP SCHEMA IF EXISTS xse5_empty CASCADE
DROP TABLE IF EXISTS xse5_t CASCADE
DROP DOMAIN IF EXISTS xse5_d
CREATE SCHEMA xse5
CREATE SCHEMA "Xse5:odd"
CREATE SCHEMA xse5_empty
CREATE TYPE xse5.mood AS ENUM ('a', 'b')
CREATE TYPE xse5.pair AS (x int, y text)
CREATE DOMAIN xse5.money5 AS numeric(5,2) CHECK (value > 0)
CREATE DOMAIN xse5.dd AS xse5.money5
CREATE DOMAIN xse5_d AS varchar(10)
CREATE TABLE xse5.zeta (id int PRIMARY KEY, m xse5.mood, p xse5.pair, d xse5.money5)
CREATE TABLE xse5.alpha (s int2, b int8, r real, f float8, o bool, t time, tz timetz, ts timestamp, dt date, c char(3), v varchar(7), n numeric(10,2), u uuid, j json, i interval, va varchar(4)[], "xmlCol" text, "a_xb" int, "a.b" int, dd xse5.dd)
CREATE VIEW xse5.vw AS SELECT id, p FROM xse5.zeta
CREATE SEQUENCE xse5.sq
CREATE TABLE "Xse5:odd"."t:1" (a int)
CREATE TABLE xse5_t (a int, b text, c xse5_d, "x:y" numeric(5,2), e bytea, f xml, g timestamptz, h int[], "Ünï" int)
INSERT INTO xse5.zeta VALUES (1, 'a', '(1,x)', 2.5), (2, null, null, null)
INSERT INTO xse5.alpha VALUES (1, 2, 1.5, 2.25, true, '01:02:03', '01:02:03+02', '2020-01-02 03:04:05.5', '2020-01-02', 'ab', 'xy', 3.14, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '{"k":1}', '1 day', '{a,b}', 'x', 5, 6, 1.25)
INSERT INTO xse5_t VALUES (1, 'x<y', 'dd', 1.5, '\x0102', '<a>q</a>', '2020-01-01 00:00:00+00', '{1,2}', 7), (2, null, null, null, null, null, null, null, null)
