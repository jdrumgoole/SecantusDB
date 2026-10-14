DROP TABLE IF EXISTS cf_t
DROP TABLE IF EXISTS cf_n
DROP TYPE IF EXISTS cf_n_t CASCADE
DROP TYPE IF EXISTS cf_c CASCADE
CREATE TYPE cf_c AS (a int, b text)
CREATE TYPE cf_n_t AS (inner_b cf_c, c int)
CREATE TABLE cf_t (id int PRIMARY KEY, v cf_c)
CREATE TABLE cf_n (id int PRIMARY KEY, w cf_n_t)
INSERT INTO cf_t VALUES (1, row(1, 'x')), (2, NULL), (3, row(NULL, 'y'))
INSERT INTO cf_n VALUES (1, row(row(2, 'p'), 3)), (2, NULL)
