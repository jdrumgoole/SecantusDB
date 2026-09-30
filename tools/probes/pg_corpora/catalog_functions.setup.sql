DROP TABLE IF EXISTS cfx_c CASCADE
DROP TABLE IF EXISTS cfx_p CASCADE
DROP SEQUENCE IF EXISTS cfx_s
DROP TABLE IF EXISTS cfx_a CASCADE
DROP TABLE IF EXISTS cfx_b CASCADE
CREATE TABLE cfx_p (id int PRIMARY KEY, n text)
CREATE TABLE cfx_c (id int PRIMARY KEY, pid int REFERENCES cfx_p(id), v numeric(5,2) CHECK (v > 0), f float8 CHECK (f >= 1.5), i int CHECK (i > -3))
CREATE INDEX cfx_c_v ON cfx_c (v DESC, i)
CREATE SEQUENCE cfx_s START 5
CREATE TABLE cfx_a (id int, opts text[])
CREATE TABLE cfx_b (id int, w int, opts text[])
INSERT INTO cfx_a VALUES (1, '{a=1}'), (2, NULL)
INSERT INTO cfx_b VALUES (1, 10, '{b=2}'), (1, 20, NULL), (2, 5, NULL)
