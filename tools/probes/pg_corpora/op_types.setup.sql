DROP TABLE IF EXISTS opt_t
DROP TABLE IF EXISTS opt_d
CREATE TABLE opt_t (s text, v varchar, n int, d date, b bool)
INSERT INTO opt_t VALUES ('1', '1', 1, '2020-01-01', true)
CREATE TABLE opt_d (id int, d date)
INSERT INTO opt_d VALUES (1, '2020-01-01'), (2, NULL), (3, 'infinity'), (4, '2019-12-31')
