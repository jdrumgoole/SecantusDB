DROP TABLE IF EXISTS mau_t CASCADE
DROP TABLE IF EXISTS mau_src CASCADE
DROP TABLE IF EXISTS mau_c CASCADE
DROP SEQUENCE IF EXISTS mau_seq
CREATE TABLE mau_t (k int primary key, v int, t text, c char(3))
INSERT INTO mau_t VALUES (1, 0, 'a', 'x'), (2, 10, 'b', 'y'), (3, 20, NULL, 'z')
CREATE TABLE mau_src (k int, n int, s text)
INSERT INTO mau_src VALUES (1, 100, 'one'), (2, 200, 'two'), (2, 201, 'two2')
CREATE SEQUENCE mau_seq
CREATE TABLE mau_c (c char(3), v varchar(3), n int)
INSERT INTO mau_c VALUES ('ab', 'ab', 1), (NULL, 'x y', 2), ('', '', 3)
