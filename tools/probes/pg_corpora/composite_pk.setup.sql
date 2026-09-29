DROP TABLE IF EXISTS cpk_child
DROP TABLE IF EXISTS cpk
CREATE TABLE cpk (a int, b text, v int, PRIMARY KEY (a, b))
INSERT INTO cpk VALUES (1, 'x', 10), (1, 'y', 20), (2, 'x', 30)
