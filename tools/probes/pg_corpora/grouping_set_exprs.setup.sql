DROP TABLE IF EXISTS gse_t CASCADE
CREATE TABLE gse_t (k int primary key, g text, z text, v int)
INSERT INTO gse_t VALUES (1, 'a', 'p', 1), (2, 'a', 'q', 2), (3, 'b', 'p', 3), (4, NULL, 'q', 4)
