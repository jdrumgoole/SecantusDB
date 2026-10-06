DROP TABLE IF EXISTS b59_s
CREATE TABLE b59_s (id int PRIMARY KEY, k int, t text, f float8, n numeric)
INSERT INTO b59_s SELECT g, CASE WHEN g % 7 = 0 THEN NULL ELSE (g * 37) % 11 END, 'r' || (g % 13), g / 3.0, ((g % 5) || '.' || repeat('0', g % 3))::numeric FROM generate_series(1, 600) g
