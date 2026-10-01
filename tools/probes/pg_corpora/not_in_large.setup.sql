DROP TABLE IF EXISTS nl15_a;
DROP TABLE IF EXISTS nl15_b;
CREATE TABLE nl15_a (id int, f float8, t text, n numeric);
CREATE TABLE nl15_b (k int, f float8, t text, n numeric);
INSERT INTO nl15_a SELECT g, g::float8 / 2, 'v' || g, g FROM generate_series(1, 60) g;
INSERT INTO nl15_a VALUES (NULL, 'NaN', NULL, NULL), (61, '-0', 'v61', 61.0);
INSERT INTO nl15_b SELECT g * 2, g::float8, 'v' || (g * 3), g * 2 FROM generate_series(1, 40) g;
INSERT INTO nl15_b VALUES (NULL, 'NaN', 'v61', NULL), (0, 0, NULL, 61);
