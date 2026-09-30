DROP TABLE IF EXISTS df_t
DROP TABLE IF EXISTS df_s
DROP TABLE IF EXISTS df_u
CREATE TABLE df_t (id int PRIMARY KEY, n int, s text)
CREATE TABLE df_s (id int PRIMARY KEY, tid int, v int)
CREATE TABLE df_u (id int PRIMARY KEY, w text)
INSERT INTO df_t VALUES (1, 10, 'a'), (2, 20, 'b'), (3, 30, 'c'), (4, 40, 'd')
INSERT INTO df_s VALUES (1, 1, 100), (2, 2, 200), (3, 9, 900)
INSERT INTO df_u VALUES (1, 'x'), (3, 'z')
DROP VIEW IF EXISTS ry_v
DROP TABLE IF EXISTS ry_o
