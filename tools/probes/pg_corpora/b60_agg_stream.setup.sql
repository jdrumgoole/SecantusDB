DROP TABLE IF EXISTS b60_a
DROP TABLE IF EXISTS b60_a0
CREATE TABLE b60_a (id int PRIMARY KEY, i int, b bigint, n numeric, t text, f bool, c char(4), pad text)
INSERT INTO b60_a SELECT g, CASE WHEN g % 11 = 0 THEN NULL ELSE g % 97 - 40 END, 9000000000000000000 - g * 3, CASE WHEN g % 7 = 0 THEN NULL ELSE (g % 13) * 1.25 + 0.001 * (g % 5) END, CASE WHEN g % 5 = 0 THEN NULL ELSE 'v' || (g * 7919 % 1009) END, g % 3 <> 0, 'c' || (g % 9), repeat('x', 200) FROM generate_series(1, 4000) g
CREATE TABLE b60_a0 (id int PRIMARY KEY, i int, n numeric)
