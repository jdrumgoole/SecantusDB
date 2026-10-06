DROP TABLE IF EXISTS b60_j
DROP TABLE IF EXISTS b60_j2
DROP TABLE IF EXISTS b60_ju
CREATE TABLE b60_j (id int PRIMARY KEY, j jsonb, k int)
INSERT INTO b60_j VALUES (1, '{"x":1}', 1), (2, '{"x":1.0}', 1), (3, '[1]', 2), (4, '[1.00]', 2), (5, '1', 3), (6, '1.0', 3), (7, '{"x":1, "y":[2.0]}', 4), (8, '{"y":[2], "x":1.000}', 4), (9, NULL, 5), (10, '"s"', 6), (11, '{}', 7), (12, '1e2', 8), (13, '100', 8)
CREATE TABLE b60_j2 (id int PRIMARY KEY, j jsonb)
INSERT INTO b60_j2 VALUES (1, '{"x":1.00}'), (2, '[1.0]'), (3, '100.0')
CREATE TABLE b60_ju (id int PRIMARY KEY, j jsonb UNIQUE)
INSERT INTO b60_ju VALUES (1, '{"x":1}')
