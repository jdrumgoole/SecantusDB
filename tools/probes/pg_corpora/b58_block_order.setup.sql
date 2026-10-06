DROP TABLE IF EXISTS b58_bo
CREATE TABLE b58_bo (id int PRIMARY KEY, k int, t text, f float8)
INSERT INTO b58_bo SELECT g, CASE WHEN g % 7 = 0 THEN NULL ELSE (g * 37) % 11 END, 'r' || (g % 13), g / 3.0 FROM generate_series(1, 600) g
