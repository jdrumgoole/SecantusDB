DROP TABLE IF EXISTS gf_t
CREATE TABLE gf_t (id int PRIMARY KEY, a text, b text, n int)
INSERT INTO gf_t VALUES (1, 'x', 'p', 1), (2, 'x', 'q', 2), (3, 'y', 'p', 3), (4, NULL, 'q', 4)
