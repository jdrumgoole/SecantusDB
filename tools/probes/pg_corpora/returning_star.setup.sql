DROP TABLE IF EXISTS rs_t CASCADE
DROP TABLE IF EXISTS rs_s CASCADE
CREATE TABLE rs_t (k int primary key, v int, t text)
INSERT INTO rs_t VALUES (1, 0, 'a'), (2, 10, 'b'), (3, 20, 'c')
CREATE TABLE rs_s (k int, n int, s text)
INSERT INTO rs_s VALUES (1, 100, 'one'), (2, 200, 'two')
