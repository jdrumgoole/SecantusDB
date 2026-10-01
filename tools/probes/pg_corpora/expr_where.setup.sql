DROP TABLE IF EXISTS ew15;
CREATE TABLE ew15 (id int PRIMARY KEY, t text, a int, b int);
INSERT INTO ew15 SELECT g, 'v' || g, g, g % 3 FROM generate_series(1, 30) g;
INSERT INTO ew15 VALUES (31, NULL, NULL, 1), (32, 'V5', 5, NULL), (33, 'mixed Case', 7, 2);
CREATE INDEX ew15_up ON ew15 (upper(t));
CREATE INDEX ew15_ab ON ew15 ((a + b));
CREATE INDEX ew15_part ON ew15 (lower(t)) WHERE a > 20;
