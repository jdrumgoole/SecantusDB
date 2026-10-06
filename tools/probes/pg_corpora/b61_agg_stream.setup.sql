DROP TABLE IF EXISTS b61_f
DROP TABLE IF EXISTS b61_f0
CREATE TABLE b61_f (id int PRIMARY KEY, i int, b bigint, n numeric, d float8, r real, s smallint, pad text)
INSERT INTO b61_f SELECT g, CASE WHEN g % 11 = 0 THEN NULL ELSE g % 97 - 40 END, 9000000000000000000 - g * 3, CASE WHEN g % 7 = 0 THEN NULL ELSE (g % 13) * 1.25 + 0.001 * (g % 5) END, CASE WHEN g % 6 = 0 THEN NULL ELSE 1.0 / g + g * 0.1 END, CASE WHEN g % 9 = 0 THEN NULL ELSE (1.0 / g + g * 0.3)::real END, (g % 300)::smallint, repeat('x', 200) FROM generate_series(1, 4000) g
CREATE TABLE b61_f0 (id int PRIMARY KEY, d float8, r real, n numeric)
